//! lynkei-bench — measures what the proxy adds, against local servers (no network noise).
//!
//!   cargo run -p lynkei-bench --release
//!
//! Prints added latency (p50/p99) for plain HTTP and decrypted HTTPS, requests/sec with
//! concurrent keep-alive connections, bulk throughput, and proxy cold-start time, each compared
//! with the direct path and with the P1 targets from PLAN.md §2.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::body::{Body, Frame};
use hyper::client::conn::http1::{self, SendRequest};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use lynkei_proxy::ProxyConfig;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const CHUNK: usize = 64 * 1024;

/// Streams `len` zero bytes in 64 KB chunks without allocating the whole body.
struct Zeros {
    left: u64,
    chunk: Bytes,
}

impl Body for Zeros {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.left == 0 {
            return Poll::Ready(None);
        }
        let n = (self.left as usize).min(CHUNK);
        self.left -= n as u64;
        Poll::Ready(Some(Ok(Frame::data(self.chunk.slice(..n)))))
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        hyper::body::SizeHint::with_exact(self.left)
    }
}

/// `/` → "ok"; `/bytes/N` → N zero bytes.
async fn origin() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let _ = s.set_nodelay(true);
            tokio::spawn(async move {
                let svc = service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let len = req
                        .uri()
                        .path()
                        .strip_prefix("/bytes/")
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(2);
                    let body = Zeros {
                        left: len,
                        chunk: Bytes::from(vec![0u8; CHUNK]),
                    };
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .header("content-type", "application/octet-stream")
                            .body(body)
                            .unwrap(),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(s), svc)
                    .await;
            });
        }
    });
    port
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    io: S,
) -> SendRequest<Empty<Bytes>> {
    let (sender, conn) = http1::handshake(TokioIo::new(io)).await.unwrap();
    tokio::spawn(conn);
    sender
}

/// A keep-alive connection: direct to the origin, through the proxy (absolute-form), or through
/// the proxy with CONNECT + TLS (decrypted by Lynkei).
#[derive(Clone, Copy, PartialEq)]
enum Path {
    Direct,
    ProxyHttp,
    ProxyHttps,
}

struct Conn {
    sender: SendRequest<Empty<Bytes>>,
    target: String,
}

async fn connect(
    path: Path,
    origin: u16,
    tls_origin: u16,
    proxy: SocketAddr,
    lynkei_ca: &rustls::pki_types::CertificateDer<'static>,
) -> Conn {
    match path {
        Path::Direct => {
            let s = TcpStream::connect(("127.0.0.1", origin)).await.unwrap();
            s.set_nodelay(true).unwrap();
            Conn {
                sender: handshake(s).await,
                target: String::new(),
            }
        }
        Path::ProxyHttp => {
            let s = TcpStream::connect(proxy).await.unwrap();
            s.set_nodelay(true).unwrap();
            Conn {
                sender: handshake(s).await,
                target: format!("http://127.0.0.1:{origin}"),
            }
        }
        Path::ProxyHttps => {
            let mut s = TcpStream::connect(proxy).await.unwrap();
            s.set_nodelay(true).unwrap();
            s.write_all(format!("CONNECT localhost:{tls_origin} HTTP/1.1\r\nHost: localhost:{tls_origin}\r\n\r\n").as_bytes()).await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut b = [0u8; 1];
                s.read_exact(&mut b).await.unwrap();
                head.push(b[0]);
            }
            let mut roots = rustls::RootCertStore::empty();
            roots.add(lynkei_ca.clone()).unwrap();
            let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
            let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
                .connect(
                    rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                    s,
                )
                .await
                .unwrap();
            Conn {
                sender: handshake(tls).await,
                target: String::new(),
            }
        }
    }
}

impl Conn {
    async fn get(&mut self, path: &str, host: &str) -> u64 {
        self.sender.ready().await.unwrap();
        let req = Request::get(format!("{}{path}", self.target))
            .header("host", host)
            .body(Empty::new())
            .unwrap();
        let resp = self.sender.send_request(req).await.unwrap();
        let mut body = resp.into_body();
        let mut n = 0u64;
        while let Some(f) = body.frame().await {
            if let Ok(d) = f.unwrap().into_data() {
                n += d.len() as u64;
            }
        }
        n
    }
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Origins: plain and TLS (the TLS origin is plain HTTP behind our own TLS terminator? no —
    // the proxy needs a real TLS server, so build one with a throwaway CA it trusts).
    let origin_port = origin().await;
    let (tls_port, origin_ca) = tls_origin().await;

    let dir = std::env::temp_dir().join(format!("lynkei-bench-{}", std::process::id()));
    let mut cfg = ProxyConfig::new("127.0.0.1:0".parse()?, &dir);
    cfg.extra_upstream_roots = vec![origin_ca];
    // `--store`: persist every event like lynkeid does (SQLite + FTS), to measure the full path.
    let with_store = std::env::args().any(|a| a == "--store");
    let store =
        lynkei_store::Store::open(&dir.join("bench.db"), lynkei_store::StoreConfig::default())?;
    let sink: Arc<dyn lynkei_proxy::EventSink> = if with_store {
        lynkei_store::StoreSink::new(store.clone(), "bench".into(), None)
    } else {
        Arc::new(|_t: lynkei_proxy::Transaction| {})
    };
    let cold = Instant::now();
    let handle = lynkei_proxy::start(cfg, sink.clone()).await?;
    let cold_start = cold.elapsed();
    let proxy = handle.local_addr;
    use rustls::pki_types::pem::PemObject;
    let lynkei_ca = rustls::pki_types::CertificateDer::from_pem_slice(handle.ca_pem().as_bytes())?;

    println!(
        "lynkei-bench · local origins · {} CPU threads · events {}\n",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        if with_store {
            "persisted to SQLite (like lynkeid)"
        } else {
            "discarded"
        }
    );

    // 1) Latency, sequential keep-alive requests.
    const N: usize = 3_000;
    let mut lat = Vec::new();
    for (name, path, host) in [
        ("direct", Path::Direct, format!("127.0.0.1:{origin_port}")),
        (
            "via Lynkei (HTTP)",
            Path::ProxyHttp,
            format!("127.0.0.1:{origin_port}"),
        ),
        (
            "via Lynkei (HTTPS, decrypted)",
            Path::ProxyHttps,
            format!("localhost:{tls_port}"),
        ),
    ] {
        let mut c = connect(path, origin_port, tls_port, proxy, &lynkei_ca).await;
        for _ in 0..200 {
            c.get("/", &host).await; // warm up (leaf cert minted, pools filled)
        }
        let mut samples = Vec::with_capacity(N);
        for _ in 0..N {
            let t = Instant::now();
            c.get("/", &host).await;
            samples.push(t.elapsed());
        }
        samples.sort();
        lat.push((name, pct(&samples, 0.5), pct(&samples, 0.99)));
    }
    let (d50, d99) = (lat[0].1, lat[0].2);
    println!("Latency per request (keep-alive, {N} sequential):");
    for (name, p50, p99) in &lat {
        println!(
            "  {name:<30} p50 {:>7.3} ms   p99 {:>7.3} ms",
            p50.as_secs_f64() * 1e3,
            p99.as_secs_f64() * 1e3
        );
    }
    let add50 = lat[1].1.saturating_sub(d50);
    let add99 = lat[1].2.saturating_sub(d99);
    let add50s = lat[2].1.saturating_sub(d50);
    println!(
        "  added by Lynkei: HTTP p50 +{:.3} ms, p99 +{:.3} ms; HTTPS p50 +{:.3} ms   (target p50 < 1 ms, p99 < 5 ms)\n",
        add50.as_secs_f64() * 1e3,
        add99.as_secs_f64() * 1e3,
        add50s.as_secs_f64() * 1e3
    );

    // 2) Requests/sec with concurrent keep-alive connections.
    println!("Requests/sec (32 keep-alive connections, 3 s):");
    let mut rps_results = Vec::new();
    for (name, path) in [
        ("direct", Path::Direct),
        ("via Lynkei (HTTP)", Path::ProxyHttp),
        ("via Lynkei (HTTPS)", Path::ProxyHttps),
    ] {
        let host = if path == Path::ProxyHttps {
            format!("localhost:{tls_port}")
        } else {
            format!("127.0.0.1:{origin_port}")
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let host = host.clone();
            let ca = lynkei_ca.clone();
            tasks.push(tokio::spawn(async move {
                let mut c = connect(path, origin_port, tls_port, proxy, &ca).await;
                let mut n = 0u64;
                while Instant::now() < deadline {
                    c.get("/", &host).await;
                    n += 1;
                }
                n
            }));
        }
        let mut total = 0;
        for t in tasks {
            total += t.await?;
        }
        let rps = total as f64 / 3.0;
        rps_results.push(rps);
        println!("  {name:<30} {rps:>9.0} req/s");
    }
    println!("  target ≥ 5,000 req/s through Lynkei\n");

    // 3) Bulk throughput.
    const BULK: u64 = 1 << 30; // 1 GiB, streamed
    println!("Throughput (1 GiB streamed, one connection):");
    for (name, path) in [
        ("direct", Path::Direct),
        ("via Lynkei (HTTP)", Path::ProxyHttp),
        ("via Lynkei (HTTPS)", Path::ProxyHttps),
    ] {
        let host = if path == Path::ProxyHttps {
            format!("localhost:{tls_port}")
        } else {
            format!("127.0.0.1:{origin_port}")
        };
        let mut c = connect(path, origin_port, tls_port, proxy, &lynkei_ca).await;
        let t = Instant::now();
        let n = c.get(&format!("/bytes/{BULK}"), &host).await;
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(n, BULK);
        println!("  {name:<30} {:>8.2} Gbit/s", n as f64 * 8.0 / secs / 1e9);
    }
    println!("  target ≥ 1 Gbit/s through Lynkei\n");

    println!(
        "Cold start to listening: {:.1} ms (target < 300 ms; includes loading the CA)",
        cold_start.as_secs_f64() * 1e3
    );
    if with_store {
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("Stored transactions: {}", store.count()?);
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// TLS origin on localhost with its own throwaway CA (returned so Lynkei can trust it upstream).
async fn tls_origin() -> (u16, rustls::pki_types::CertificateDer<'static>) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
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
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![leaf.der().clone(), ca_cert.der().clone()], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let _ = s.set_nodelay(true);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    let svc = service_fn(|req: Request<hyper::body::Incoming>| async move {
                        let len = req
                            .uri()
                            .path()
                            .strip_prefix("/bytes/")
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(2);
                        Ok::<_, std::convert::Infallible>(Response::new(Zeros {
                            left: len,
                            chunk: Bytes::from(vec![0u8; CHUNK]),
                        }))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), svc)
                        .await;
                }
            });
        }
    });
    (port, ca_cert.der().clone())
}
