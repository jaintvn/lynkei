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
fn origin_pki() -> (
    CertificateDer<'static>,
    Vec<CertificateDer<'static>>,
    PrivateKeyDer<'static>,
) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(vec![]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    (
        ca_cert.der().clone(),
        vec![leaf.der().clone(), ca_cert.der().clone()],
        key,
    )
}

type OriginBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// Echoes the path and the HTTP version the origin saw. `/grpc` answers like a gRPC server:
/// a body followed by `grpc-status` trailers.
async fn origin_handler(
    mut req: Request<hyper::body::Incoming>,
) -> Result<Response<OriginBody>, Infallible> {
    use http_body_util::BodyExt;
    if req.uri().path() == "/ws" {
        return Ok(websocket_echo(&mut req));
    }
    let version = if req.version() == http::Version::HTTP_2 {
        "HTTP/2"
    } else {
        "HTTP/1.1"
    };
    // Be as strict as Google's front end: HTTP/2 requests must carry the host only in :authority.
    if req.version() == http::Version::HTTP_2 && req.headers().contains_key("host") {
        return Ok(Response::builder()
            .status(400)
            .body(Full::new(Bytes::from_static(b"host header on h2")).boxed())
            .unwrap());
    }
    if req.uri().path() == "/grpc" {
        let te_ok = req.headers().get("te").is_some_and(|v| v == "trailers");
        let mut trailers = http::HeaderMap::new();
        trailers.insert(
            "grpc-status",
            if te_ok {
                "0".parse().unwrap()
            } else {
                "13".parse().unwrap()
            },
        );
        trailers.insert("grpc-message", "ok".parse().unwrap());
        let body = Full::new(Bytes::from_static(b"\x00\x00\x00\x00\x02hi"))
            .with_trailers(async move { Some(Ok(trailers)) });
        return Ok(Response::builder()
            .header("content-type", "application/grpc")
            .body(body.boxed())
            .unwrap());
    }
    let body = format!(
        r#"{{"path":"{}","items":[1,2,3],"version":"{}"}}"#,
        req.uri(),
        version
    );
    Ok(Response::builder()
        .header("content-type", "application/json")
        .header("x-origin", "test")
        .body(Full::new(Bytes::from(body)).boxed())
        .unwrap())
}

/// Serves HTTP/1.1 or HTTP/2 (ALPN on TLS, prior knowledge on plain TCP).
async fn serve_origin<IO>(io: IO)
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(io), service_fn(origin_handler))
        .await;
}

/// Starts an HTTPS origin on localhost (h2 + http/1.1); returns (port, origin CA).
async fn start_https_origin() -> (u16, CertificateDer<'static>) {
    let (ca, chain, key) = origin_pki();
    let mut cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    serve_origin(tls).await;
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
            tokio::spawn(serve_origin(s));
        }
    });
    port
}

async fn start_proxy(
    origin_ca: CertificateDer<'static>,
    passthrough: &[&str],
) -> (relay_proxy::ProxyHandle, Log, CertificateDer<'static>) {
    let dir = std::env::temp_dir().join(format!(
        "relay-e2e-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let mut cfg = ProxyConfig::new("127.0.0.1:0".parse().unwrap(), &dir);
    cfg.extra_upstream_roots = vec![origin_ca];
    cfg.passthrough_hosts = passthrough.iter().map(|s| s.to_string()).collect();
    let log: Log = Arc::default();
    let sink_log = log.clone();
    let handle = relay_proxy::start(
        cfg,
        Arc::new(move |t: Transaction| {
            let mut log = sink_log.lock().unwrap();
            match log.iter_mut().find(|e| e.id == t.id) {
                Some(e) => *e = t,
                None => log.push(t),
            }
        }),
    )
    .await
    .unwrap();
    let relay_ca = CertificateDer::from_pem_slice(handle.ca_pem().as_bytes()).unwrap();
    (handle, log, relay_ca)
}

fn rand_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Waits until `n` transactions reached a final state, and returns those.
async fn wait_for(log: &Log, n: usize) -> Vec<Transaction> {
    let finals = || -> Vec<Transaction> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|t| t.state != TransactionState::InProgress)
            .cloned()
            .collect()
    };
    for _ in 0..150 {
        if finals().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    finals()
}

#[tokio::test]
async fn decrypts_https_when_client_trusts_relay_ca() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, relay_ca) = start_proxy(origin_ca, &[]).await;
    let url = format!("https://localhost:{origin_port}/v2/products?page=2");

    // The client trusts ONLY the Relay CA, so success proves Relay terminated TLS.
    let res = fetch_via_proxy(proxy.local_addr, &url, &[relay_ca])
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert!(String::from_utf8_lossy(&res.body).contains(r#""items":[1,2,3]"#));

    let txs = wait_for(&log, 1).await;
    let t = &txs[0];
    assert_eq!(t.state, TransactionState::Complete);
    assert!(t.decrypted);
    assert_eq!(
        (t.method.as_str(), t.scheme.as_str(), t.host.as_str()),
        ("GET", "https", "localhost")
    );
    assert_eq!(t.path, "/v2/products?page=2");
    assert_eq!(t.status, Some(200));
    assert_eq!(t.response_header("x-origin"), Some("test"));
    assert!(
        t.response_body_preview
            .as_deref()
            .unwrap()
            .contains("items")
    );
    assert_eq!(
        (t.http_version.as_str(), t.upstream_http_version.as_deref()),
        ("HTTP/1.1", Some("HTTP/2"))
    );
    assert!(
        String::from_utf8_lossy(&res.body).contains(r#""version":"HTTP/2""#),
        "origin was reached over h2"
    );
}

#[tokio::test]
async fn records_tls_failure_when_client_pins_the_real_certificate() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, _relay_ca) = start_proxy(origin_ca.clone(), &[]).await;
    let url = format!("https://localhost:{origin_port}/pinned");

    // A "pinning" client trusts only the origin's real CA, so it must reject Relay's certificate.
    assert!(
        fetch_via_proxy(proxy.local_addr, &url, &[origin_ca])
            .await
            .is_err()
    );
    let txs = wait_for(&log, 1).await;
    assert_eq!(txs[0].state, TransactionState::Failed);
    assert_eq!(txs[0].method, "CONNECT");
    assert!(
        txs[0].error.as_deref().unwrap().contains("pin"),
        "{:?}",
        txs[0].error
    );
}

#[tokio::test]
async fn passthrough_hosts_are_tunneled_not_decrypted() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, _relay_ca) = start_proxy(origin_ca.clone(), &["localhost"]).await;
    let url = format!("https://localhost:{origin_port}/secret");

    // Real origin certificate reaches the client untouched.
    let res = fetch_via_proxy(proxy.local_addr, &url, &[origin_ca])
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    // Visible while the tunnel is still open (P0 only showed it after it closed).
    for _ in 0..50 {
        if !log.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(log.lock().unwrap()[0].state, TransactionState::InProgress);
    drop(proxy); // final event when the tunnel closes
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

    let res = fetch_via_proxy(
        proxy.local_addr,
        &format!("http://127.0.0.1:{http_port}/health"),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(res.status, 200);
    let txs = wait_for(&log, 1).await;
    assert_eq!(
        (
            txs[0].scheme.as_str(),
            txs[0].path.as_str(),
            txs[0].decrypted
        ),
        ("http", "/health", false)
    );

    let ca = fetch_via_proxy(proxy.local_addr, "http://relay.ca/", &[])
        .await
        .unwrap();
    assert_eq!(ca.status, 200);
    assert!(String::from_utf8_lossy(&ca.body).starts_with("-----BEGIN CERTIFICATE-----"));
    let _: SocketAddr = proxy.local_addr;
}

/// Simulates VPN capture: the app's connection arrives as a raw stream aimed at an IP address,
/// with no CONNECT. The hostname must come from SNI.
#[tokio::test]
async fn transparent_tls_is_decrypted_using_sni() {
    use http_body_util::{BodyExt, Empty};
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, relay_ca) = start_proxy(origin_ca, &[]).await;
    let handler = proxy.transparent();

    let (app_side, engine_side) = tokio::io::duplex(64 * 1024);
    let dst: SocketAddr = format!("127.0.0.1:{origin_port}").parse().unwrap();
    handler.handle(engine_side, dst, "10.111.0.2:40000".parse().unwrap());

    let mut roots = rustls::RootCertStore::empty();
    roots.add(relay_ca).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            app_side,
        )
        .await
        .expect("app trusts Relay CA");
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(conn);
    let req = Request::get("/v2/recommendations")
        .header("host", format!("localhost:{origin_port}"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("/v2/recommendations"));

    let t = wait_for(&log, 1).await.remove(0);
    assert!(t.decrypted);
    assert_eq!(
        (t.host.as_str(), t.path.as_str(), t.client_addr.as_str()),
        ("localhost", "/v2/recommendations", "10.111.0.2:40000")
    );
}

#[tokio::test]
async fn transparent_plain_http_uses_host_header() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let http_port = start_http_origin().await;
    let (_p, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;

    let (mut app_side, engine_side) = tokio::io::duplex(64 * 1024);
    let dst: SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();
    proxy
        .transparent()
        .handle(engine_side, dst, "10.111.0.2:40001".parse().unwrap());
    app_side
        .write_all(format!("GET /health?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{http_port}\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    app_side.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");

    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(
        (t.scheme.as_str(), t.path.as_str(), t.state),
        ("http", "/health?x=1", TransactionState::Complete)
    );
}

// ---------- streaming ----------

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Body backed by a channel, so the origin can send chunks over time (SSE-style).
struct ChannelBody(tokio::sync::mpsc::Receiver<Bytes>);
impl hyper::body::Body for ChannelBody {
    type Data = Bytes;
    type Error = BoxError;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BoxError>>> {
        self.0
            .poll_recv(cx)
            .map(|o| o.map(|b| Ok(hyper::body::Frame::data(b))))
    }
}

enum StreamOrigin {
    Chunks(ChannelBody),
    Full(Full<Bytes>),
}
impl hyper::body::Body for StreamOrigin {
    type Data = Bytes;
    type Error = BoxError;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BoxError>>> {
        match self.get_mut() {
            StreamOrigin::Chunks(c) => std::pin::Pin::new(c).poll_frame(cx),
            StreamOrigin::Full(f) => std::pin::Pin::new(f)
                .poll_frame(cx)
                .map_err(|never| match never {}),
        }
    }
}

const BIG: usize = 5 * 1024 * 1024;

/// /sse: two events 400 ms apart, then a long pause (until the client leaves); /big: 5 MB of text.
async fn start_streaming_origin() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let svc = service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let resp = match req.uri().path() {
                        "/sse" | "/sse-forever" => {
                            let forever = req.uri().path() == "/sse-forever";
                            let (tx, rx) = tokio::sync::mpsc::channel(4);
                            tokio::spawn(async move {
                                let _ = tx.send(Bytes::from_static(b"data: one\n\n")).await;
                                tokio::time::sleep(Duration::from_millis(400)).await;
                                let _ = tx.send(Bytes::from_static(b"data: two\n\n")).await;
                                if forever {
                                    tokio::time::sleep(Duration::from_secs(30)).await;
                                }
                            });
                            Response::builder()
                                .header("content-type", "text/event-stream")
                                .body(StreamOrigin::Chunks(ChannelBody(rx)))
                                .unwrap()
                        }
                        _ => Response::builder()
                            .header("content-type", "text/plain")
                            .body(StreamOrigin::Full(Full::new(Bytes::from(vec![b'x'; BIG]))))
                            .unwrap(),
                    };
                    Ok::<_, Infallible>(resp)
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(s), svc)
                    .await;
            });
        }
    });
    port
}

async fn plain_client(
    proxy: SocketAddr,
) -> hyper::client::conn::http1::SendRequest<http_body_util::Empty<Bytes>> {
    let s = tokio::net::TcpStream::connect(proxy).await.unwrap();
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(s))
        .await
        .unwrap();
    tokio::spawn(conn);
    sender
}

#[tokio::test]
async fn streams_responses_as_they_arrive() {
    use http_body_util::BodyExt;
    let origin = start_streaming_origin().await;
    let (_tls, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;
    let mut client = plain_client(proxy.local_addr).await;

    let started = std::time::Instant::now();
    let req = Request::get(format!("http://127.0.0.1:{origin}/sse"))
        .body(http_body_util::Empty::new())
        .unwrap();
    let mut body = client.send_request(req).await.unwrap().into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let first_at = started.elapsed();
    assert_eq!(&first[..], b"data: one\n\n");
    assert!(
        first_at < Duration::from_millis(300),
        "first event must not wait for the whole stream ({first_at:?})"
    );
    {
        let open = log.lock().unwrap().clone();
        assert_eq!(open.len(), 1, "visible while still streaming");
        assert_eq!(
            (open[0].state, open[0].status),
            (TransactionState::InProgress, Some(200))
        );
    }

    let rest = body.collect().await.unwrap().to_bytes();
    assert_eq!(&rest[..], b"data: two\n\n");
    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(
        (t.state, t.response_body_size),
        (TransactionState::Complete, 22)
    );
    assert!(t.duration_ms >= 400);
    assert_eq!(
        t.response_body_preview.as_deref(),
        Some("data: one\n\ndata: two\n\n")
    );
}

#[tokio::test]
async fn large_bodies_are_forwarded_whole_but_captured_partially() {
    use http_body_util::BodyExt;
    let origin = start_streaming_origin().await;
    let (_tls, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;
    let mut client = plain_client(proxy.local_addr).await;

    let req = Request::get(format!("http://127.0.0.1:{origin}/big"))
        .body(http_body_util::Empty::new())
        .unwrap();
    let body = client
        .send_request(req)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body.len(), BIG, "client receives every byte");

    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(
        (t.state, t.response_body_size),
        (TransactionState::Complete, BIG as u64)
    );
    let p = t.response_body_preview.unwrap();
    assert!(
        p.len() < 300 * 1024,
        "capture is bounded, not the whole 5 MB"
    );
    assert!(p.contains("truncated"), "preview says it is partial");
}

#[tokio::test]
async fn client_hanging_up_mid_stream_is_recorded() {
    use http_body_util::BodyExt;
    let origin = start_streaming_origin().await;
    let (_tls, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;
    let mut client = plain_client(proxy.local_addr).await;

    let req = Request::get(format!("http://127.0.0.1:{origin}/sse-forever"))
        .body(http_body_util::Empty::new())
        .unwrap();
    let mut body = client.send_request(req).await.unwrap().into_body();
    let _ = body.frame().await;
    drop(body);
    drop(client);

    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(t.state, TransactionState::Failed);
    assert!(
        t.error.as_deref().unwrap().contains("interrupted"),
        "{:?}",
        t.error
    );
    assert!(t.response_body_size >= 11);
}

// ---------- HTTP/2 ----------

/// CONNECT through the proxy, then TLS with the given ALPN list, trusting only `relay_ca`.
async fn tls_through_proxy(
    proxy: SocketAddr,
    port: u16,
    relay_ca: CertificateDer<'static>,
    alpn: &[&[u8]],
) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(proxy).await.unwrap();
    s.write_all(
        format!("CONNECT localhost:{port} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await.unwrap();
        head.push(b[0]);
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(relay_ca).unwrap();
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            s,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn http2_app_to_http2_origin_is_decrypted() {
    use http_body_util::{BodyExt, Empty};
    let (port, origin_ca) = start_https_origin().await;
    let (proxy, log, relay_ca) = start_proxy(origin_ca, &[]).await;
    let tls = tls_through_proxy(proxy.local_addr, port, relay_ca, &[b"h2", b"http/1.1"]).await;
    assert_eq!(
        tls.get_ref().1.alpn_protocol(),
        Some(&b"h2"[..]),
        "Relay offers h2 to the app"
    );

    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(tls),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    let req = Request::get(format!("https://localhost:{port}/v2/products?page=2"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(
        (resp.status().as_u16(), resp.version()),
        (200, http::Version::HTTP_2)
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains(r#""version":"HTTP/2""#));

    let t = wait_for(&log, 1).await.remove(0);
    assert!(t.decrypted);
    assert_eq!(t.path, "/v2/products?page=2");
    assert_eq!(
        (t.http_version.as_str(), t.upstream_http_version.as_deref()),
        ("HTTP/2", Some("HTTP/2"))
    );
}

#[tokio::test]
async fn grpc_trailers_survive_the_proxy() {
    use http_body_util::{BodyExt, Empty};
    let (port, origin_ca) = start_https_origin().await;
    let (proxy, log, relay_ca) = start_proxy(origin_ca, &[]).await;
    let tls = tls_through_proxy(proxy.local_addr, port, relay_ca, &[b"h2"]).await;
    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(tls),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    let req = Request::post(format!("https://localhost:{port}/grpc"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let collected = sender
        .send_request(req)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    let trailers = collected
        .trailers()
        .cloned()
        .expect("trailers reach the app");
    assert_eq!(
        trailers.get("grpc-status").unwrap(),
        "0",
        "te: trailers reached the server and trailers came back"
    );
    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!((t.state, t.status), (TransactionState::Complete, Some(200)));
}

/// gRPC in development often runs h2c (HTTP/2 without TLS). Through the VPN it must stay h2c.
#[tokio::test]
async fn transparent_h2c_prior_knowledge() {
    use http_body_util::{BodyExt, Empty};
    let http_port = start_http_origin().await;
    let (_p, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;

    let (app_side, engine_side) = tokio::io::duplex(256 * 1024);
    let dst: SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();
    proxy
        .transparent()
        .handle(engine_side, dst, "10.111.0.2:40100".parse().unwrap());
    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(app_side),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    let req = Request::get(format!("http://127.0.0.1:{http_port}/h2c-check"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.version(), http::Version::HTTP_2);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&body).contains(r#""version":"HTTP/2""#),
        "origin saw h2c"
    );

    let finals = wait_for(&log, 1).await;
    if finals.is_empty() {
        for t in log.lock().unwrap().iter() {
            eprintln!(
                "DEBUG {:?} {} {} {:?} up={:?} err={:?}",
                t.state, t.method, t.path, t.status, t.upstream_http_version, t.error
            );
        }
    }
    let t = finals.into_iter().next().expect("final event");
    assert_eq!(
        (
            t.scheme.as_str(),
            t.http_version.as_str(),
            t.upstream_http_version.as_deref()
        ),
        ("http", "HTTP/2", Some("HTTP/2"))
    );
}

/// A non-HTTP, non-TLS server that answers 1000 bytes and then resets the connection (RST), like
/// many real servers and apps do. Tunnel byte counts must survive the abrupt close.
#[tokio::test]
async fn tunnel_byte_counts_survive_an_abrupt_close() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut hello = [0u8; 5];
        s.read_exact(&mut hello).await.unwrap();
        s.write_all(&[7u8; 1000]).await.unwrap();
        s.set_zero_linger().unwrap(); // close with RST instead of FIN
        drop(s);
    });
    let (_p, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;

    let mut c = tokio::net::TcpStream::connect(proxy.local_addr)
        .await
        .unwrap();
    c.write_all(
        format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        c.read_exact(&mut b).await.unwrap();
        head.push(b[0]);
    }
    c.write_all(b"hello").await.unwrap();
    let mut got = vec![0u8; 1000];
    c.read_exact(&mut got).await.unwrap();
    drop(c); // the app closes its side too

    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(t.state, TransactionState::Tunneled);
    assert_eq!(
        (t.request_body_size, t.response_body_size),
        (5, 1000),
        "bytes counted despite the reset"
    );
}

/// The app leaves, but the server never closes its side (Apple's servers wait for their 60 s idle
/// timeout). The tunnel must finish after the half-close grace period, not when the server times out.
#[tokio::test]
async fn tunnel_closes_after_grace_when_server_lingers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut hello = [0u8; 5];
        s.read_exact(&mut hello).await.unwrap();
        s.write_all(b"world").await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await; // never closes in time
    });
    let (_p, origin_ca) = start_https_origin().await;
    let dir = std::env::temp_dir().join(format!("relay-e2e-grace-{}", rand_suffix()));
    let mut cfg = ProxyConfig::new("127.0.0.1:0".parse().unwrap(), &dir);
    cfg.extra_upstream_roots = vec![origin_ca];
    cfg.tunnel_half_close_grace = Duration::from_millis(300);
    let log: Log = Arc::default();
    let sink_log = log.clone();
    let proxy = relay_proxy::start(
        cfg,
        Arc::new(move |t: Transaction| {
            let mut l = sink_log.lock().unwrap();
            match l.iter_mut().find(|e| e.id == t.id) {
                Some(e) => *e = t,
                None => l.push(t),
            }
        }),
    )
    .await
    .unwrap();

    let mut c = tokio::net::TcpStream::connect(proxy.local_addr)
        .await
        .unwrap();
    c.write_all(
        format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        c.read_exact(&mut b).await.unwrap();
        head.push(b[0]);
    }
    c.write_all(b"hello").await.unwrap();
    let mut got = [0u8; 5];
    c.read_exact(&mut got).await.unwrap();
    let left_at = std::time::Instant::now();
    drop(c); // the app is done

    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(
        (t.state, t.request_body_size, t.response_body_size),
        (TransactionState::Tunneled, 5, 5)
    );
    assert!(
        left_at.elapsed() < Duration::from_secs(2),
        "closed after the grace period, took {:?}",
        left_at.elapsed()
    );
}

#[tokio::test]
async fn busy_listen_port_falls_back_to_the_next_one() {
    // Use the non-ephemeral range: macOS hands out ephemeral ports sequentially, so the ports
    // right after an OS-chosen one are often taken by parallel tests' own connections.
    let (_taken, port) = (21_000..22_000u16)
        .step_by(20)
        .find_map(|p| {
            std::net::TcpListener::bind(("127.0.0.1", p))
                .ok()
                .map(|l| (l, p))
        })
        .expect("a free test port");
    let dir = std::env::temp_dir().join(format!("relay-e2e-port-{}", rand_suffix()));
    let cfg = ProxyConfig::new(format!("127.0.0.1:{port}").parse().unwrap(), &dir);
    let proxy = relay_proxy::start(cfg, Arc::new(|_t: Transaction| {}))
        .await
        .unwrap_or_else(|e| panic!("should start despite the busy port: {e:#}"));
    assert_ne!(proxy.local_addr.port(), port);
    assert!(proxy.local_addr.port() > port && proxy.local_addr.port() <= port + 10);
}

// ---------- WebSocket ----------

/// Origin side of `/ws`: accept the upgrade and echo text ("echo: …") and binary frames.
fn websocket_echo(req: &mut Request<hyper::body::Incoming>) -> Response<OriginBody> {
    use futures_util::{SinkExt, StreamExt};
    use http_body_util::BodyExt;
    use tokio_tungstenite::tungstenite::{Message, handshake::derive_accept_key, protocol::Role};
    let key = req
        .headers()
        .get("sec-websocket-key")
        .expect("client key")
        .clone();
    let on = hyper::upgrade::on(req);
    tokio::spawn(async move {
        let up = on.await.expect("origin upgrade");
        let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
            TokioIo::new(up),
            Role::Server,
            None,
        )
        .await;
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Text(t) => ws
                    .send(Message::Text(format!("echo: {t}").into()))
                    .await
                    .unwrap(),
                Message::Binary(b) => ws.send(Message::Binary(b)).await.unwrap(),
                Message::Close(_) => break,
                _ => {}
            }
        }
    });
    Response::builder()
        .status(101)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-accept", derive_accept_key(key.as_bytes()))
        .body(Full::new(Bytes::new()).boxed())
        .unwrap()
}

/// Talks to `/ws` over `io`: text, binary, close. Returns what came back.
async fn websocket_conversation<S>(io: S, url: &str) -> Vec<String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, resp) = tokio_tungstenite::client_async(url, io)
        .await
        .expect("handshake through Relay");
    assert_eq!(resp.status(), 101);
    ws.send(Message::Text("hello".into())).await.unwrap();
    let mut got = vec![ws.next().await.unwrap().unwrap().to_string()];
    ws.send(Message::Binary(Bytes::from_static(&[1, 2, 3])))
        .await
        .unwrap();
    got.push(format!("{} bytes", ws.next().await.unwrap().unwrap().len()));
    ws.close(None).await.unwrap();
    got
}

fn assert_ws_recorded(t: &Transaction) {
    use relay_proxy::{WsDirection::*, WsKind::*};
    assert_eq!((t.state, t.status), (TransactionState::Complete, Some(101)));
    let m = &t.websocket_messages;
    let find = |d, k, p: Option<&str>| {
        m.iter()
            .any(|x| x.direction == d && x.kind == k && (p.is_none() || x.preview.as_deref() == p))
    };
    assert!(
        find(Send, Text, Some("hello")),
        "sent text recorded (unmasked): {m:?}"
    );
    assert!(
        find(Receive, Text, Some("echo: hello")),
        "received text recorded: {m:?}"
    );
    assert!(
        m.iter()
            .any(|x| x.direction == Send && x.kind == Binary && x.size == 3),
        "binary size recorded"
    );
    assert!(find(Send, Close, None), "close recorded");
}

#[tokio::test]
async fn websocket_over_decrypted_tls_is_relayed_and_recorded() {
    let (port, origin_ca) = start_https_origin().await;
    let (proxy, log, relay_ca) = start_proxy(origin_ca, &[]).await;
    let tls = tls_through_proxy(proxy.local_addr, port, relay_ca, &[b"http/1.1"]).await;
    let got = websocket_conversation(tls, &format!("wss://localhost:{port}/ws")).await;
    assert_eq!(got, vec!["echo: hello".to_string(), "3 bytes".to_string()]);

    let t = wait_for(&log, 1).await.remove(0);
    assert!(t.decrypted);
    assert_eq!(t.path, "/ws");
    assert_ws_recorded(&t);
}

#[tokio::test]
async fn websocket_through_the_vpn_path_is_relayed_and_recorded() {
    let http_port = start_http_origin().await;
    let (_p, origin_ca) = start_https_origin().await;
    let (proxy, log, _ca) = start_proxy(origin_ca, &[]).await;
    let (app_side, engine_side) = tokio::io::duplex(256 * 1024);
    let dst: SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();
    proxy
        .transparent()
        .handle(engine_side, dst, "10.111.0.2:40200".parse().unwrap());
    let got = websocket_conversation(app_side, &format!("ws://127.0.0.1:{http_port}/ws")).await;
    assert_eq!(got[0], "echo: hello");

    let t = wait_for(&log, 1).await.remove(0);
    assert_eq!(t.scheme, "http");
    assert_ws_recorded(&t);
}

/// A phone opens http://<mac>:<port>/setup straight at the proxy (origin-form, no CONNECT).
#[tokio::test]
async fn phones_can_open_the_setup_pages_on_the_proxy_port() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_p, origin_ca) = start_https_origin().await;
    let (proxy, _log, _ca) = start_proxy(origin_ca, &[]).await;
    let get = |path: &'static str| async move {
        let mut c = tokio::net::TcpStream::connect(proxy.local_addr)
            .await
            .unwrap();
        c.write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: jains-mbp.local:8888\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut out = Vec::new();
        c.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    };
    let setup = get("/setup").await;
    assert!(
        setup.starts_with("HTTP/1.1 200")
            && setup.contains("http://jains-mbp.local:8888/proxy.pac")
    );
    let cfg = get("/relay.mobileconfig").await;
    assert!(
        cfg.contains("application/x-apple-aspen-config") && cfg.contains("com.apple.security.root")
    );
    assert!(
        get("/proxy.pac")
            .await
            .contains("PROXY jains-mbp.local:8888; DIRECT")
    );
    assert!(get("/ca.pem").await.contains("BEGIN CERTIFICATE"));
    assert!(get("/nope").await.starts_with("HTTP/1.1 404"));
}

/// An app that refuses Relay's certificate (pinning, or an Android app that ignores user CAs) must
/// not stay broken: after the first refusal Relay stops intercepting that host.
#[tokio::test]
async fn a_host_that_refuses_the_relay_ca_is_passed_through_afterwards() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let (proxy, log, _relay_ca) = start_proxy(origin_ca.clone(), &[]).await;
    let url = format!("https://localhost:{origin_port}/needs-the-real-cert");

    // 1st attempt: the app only trusts the origin's real certificate → it refuses Relay's.
    assert!(
        fetch_via_proxy(proxy.local_addr, &url, std::slice::from_ref(&origin_ca))
            .await
            .is_err()
    );
    // 2nd attempt (the app's retry): Relay now tunnels it, so the REAL certificate reaches the app.
    let res = fetch_via_proxy(proxy.local_addr, &url, &[origin_ca])
        .await
        .expect("the retry must work untouched");
    assert_eq!(res.status, 200);

    drop(proxy);
    let txs = wait_for(&log, 2).await;
    assert_eq!(txs[0].state, TransactionState::Failed);
    assert!(
        txs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("passes this host through"),
        "{:?}",
        txs[0].error
    );
    assert_eq!(txs[1].state, TransactionState::Tunneled);
}

#[tokio::test]
async fn auto_passthrough_can_be_turned_off() {
    let (origin_port, origin_ca) = start_https_origin().await;
    let dir = std::env::temp_dir().join(format!("relay-e2e-noauto-{}", rand_suffix()));
    let mut cfg = ProxyConfig::new("127.0.0.1:0".parse().unwrap(), &dir);
    cfg.extra_upstream_roots = vec![origin_ca.clone()];
    cfg.auto_passthrough = false;
    let proxy = relay_proxy::start(cfg, Arc::new(|_t: Transaction| {}))
        .await
        .unwrap();
    let url = format!("https://localhost:{origin_port}/x");
    for _ in 0..2 {
        assert!(
            fetch_via_proxy(proxy.local_addr, &url, std::slice::from_ref(&origin_ca))
                .await
                .is_err(),
            "still intercepted, so still refused"
        );
    }
}
