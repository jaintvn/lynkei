//! End-to-end tests: a real local HTTPS origin, the Relay proxy in front of it, and a client
//! that trusts only the roots we give it. Fully offline.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use relay_proxy::client::fetch_via_proxy;
use relay_proxy::{CertificateDer, ProxyConfig, Transaction, TransactionState};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

type Log = Arc<Mutex<Vec<Transaction>>>;

/// A CA + "localhost" leaf for the fake origin server.
fn origin_pki() -> (CertificateDer<'static>, Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(vec![]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".into()]).unwrap().signed_by(&leaf_key, &issuer).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    (ca_cert.der().clone(), vec![leaf.der().clone(), ca_cert.der().clone()], key)
}

async fn origin_handler(req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let body = format!(r#"{{"path":"{}","items":[1,2,3]}}"#, req.uri());
    Ok(Response::builder()
        .header("content-type", "application/json")
        .header("x-origin", "test")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

/// Starts an HTTPS origin on localhost; returns (port, origin CA).
async fn start_https_origin() -> (u16, CertificateDer<'static>) {
    let (ca, chain, key) = origin_pki();
    let cfg = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service_fn(origin_handler))
                        .await;
                }
            });
        }
    });
    (port, ca)
}

async fn start_http_origin() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(s), service_fn(origin_handler))
                    .await;
            });
        }
    });
    port
}

async fn start_proxy(origin_ca: CertificateDer<'static>, passthrough: &[&str]) -> (relay_proxy::ProxyHandle, Log, CertificateDer<'static>) {
    let dir = std::env::temp_dir().join(format!("relay-e2e-{}-{}", std::process::id(), rand_suffix()));
    let mut cfg = ProxyConfig::new("127.0.0.1:0".parse().unwrap(), &dir);
    cfg.extra_upstream_roots = vec![origin_ca];
    cfg.passthrough_hosts = passthrough.iter().map(|s| s.to_string()).collect();
    let log: Log = Arc::default();
    let sink_log = log.clone();
    let handle = relay_proxy::start(cfg, Arc::new(move |t: Transaction| sink_log.lock().unwrap().push(t)))
        .await
        .unwrap();
    let relay_ca = CertificateDer::from_pem_slice(handle.ca_pem().as_bytes()).unwrap();
    (handle, log, relay_ca)
}

fn rand_suffix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

async fn wait_for(log: &Log, n: usize) -> Vec<Transaction> {
    for _ in 0..100 {
        if log.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    log.lock().unwrap().clone()
}

#[tokio::test]
async fn decrypts_https_when_client_trusts_relay_ca() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, relay_ca) = start_proxy(origin_ca, &[]).await;
    let url = format!("https://localhost:{origin_port}/v2/products?page=2");

    // The client trusts ONLY the Relay CA, so success proves Relay terminated TLS.
    let res = fetch_via_proxy(proxy.local_addr, &url, &[relay_ca]).await.unwrap();
    assert_eq!(res.status, 200);
    assert!(String::from_utf8_lossy(&res.body).contains(r#""items":[1,2,3]"#));

    let txs = wait_for(&log, 1).await;
    let t = &txs[0];
    assert_eq!(t.state, TransactionState::Complete);
    assert!(t.decrypted);
    assert_eq!((t.method.as_str(), t.scheme.as_str(), t.host.as_str()), ("GET", "https", "localhost"));
    assert_eq!(t.path, "/v2/products?page=2");
    assert_eq!(t.status, Some(200));
    assert_eq!(t.response_header("x-origin"), Some("test"));
    assert!(t.response_body_preview.as_deref().unwrap().contains("items"));
}

#[tokio::test]
async fn records_tls_failure_when_client_pins_the_real_certificate() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, _relay_ca) = start_proxy(origin_ca.clone(), &[]).await;
    let url = format!("https://localhost:{origin_port}/pinned");

    // A "pinning" client trusts only the origin's real CA, so it must reject Relay's certificate.
    assert!(fetch_via_proxy(proxy.local_addr, &url, &[origin_ca]).await.is_err());
    let txs = wait_for(&log, 1).await;
    assert_eq!(txs[0].state, TransactionState::Failed);
    assert_eq!(txs[0].method, "CONNECT");
    assert!(txs[0].error.as_deref().unwrap().contains("pin"), "{:?}", txs[0].error);
}

#[tokio::test]
async fn passthrough_hosts_are_tunneled_not_decrypted() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, _relay_ca) = start_proxy(origin_ca.clone(), &["localhost"]).await;
    let url = format!("https://localhost:{origin_port}/secret");

    // Real origin certificate reaches the client untouched.
    let res = fetch_via_proxy(proxy.local_addr, &url, &[origin_ca]).await.unwrap();
    assert_eq!(res.status, 200);
    drop(proxy); // tunnel is recorded when it closes
    let txs = wait_for(&log, 1).await;
    assert_eq!(txs[0].state, TransactionState::Tunneled);
    assert!(!txs[0].decrypted);
    assert!(txs[0].response_body_size > 0);
}

#[tokio::test]
async fn forwards_plain_http_and_serves_ca_download() {
    let http_port = start_http_origin().await;
    let (_tls_port, origin_ca) = start_https_origin().await;
    let (proxy, log, _relay_ca) = start_proxy(origin_ca, &[]).await;

    let res = fetch_via_proxy(proxy.local_addr, &format!("http://127.0.0.1:{http_port}/health"), &[]).await.unwrap();
    assert_eq!(res.status, 200);
    let txs = wait_for(&log, 1).await;
    assert_eq!((txs[0].scheme.as_str(), txs[0].path.as_str(), txs[0].decrypted), ("http", "/health", false));

    let ca = fetch_via_proxy(proxy.local_addr, "http://relay.ca/", &[]).await.unwrap();
    assert_eq!(ca.status, 200);
    assert!(String::from_utf8_lossy(&ca.body).starts_with("-----BEGIN CERTIFICATE-----"));
    let _: SocketAddr = proxy.local_addr;
}
