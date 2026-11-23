//! Compose / Repeat: sends a request of the user's own making (usually an edited copy of a
//! captured one) *through* the running proxy, so it shows up in the list like any other request
//! and rules and breakpoints apply to it.
//!
//! Only HTTP/1.1 is spoken to the proxy; the proxy talks to the real server in whatever protocol
//! it negotiates. Redirects are not followed.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Bodies larger than this are not read back in full (the size is still counted).
const MAX_PREVIEW: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ComposeRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ComposeResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Text of the body (decompressed when gzip/deflate), `None` for binary bodies.
    pub body_preview: Option<String>,
    pub body_size: u64,
    pub duration_ms: u64,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Headers that describe the connection or the body and are set again for the new request.
fn is_framing(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "upgrade"
            | "te"
            | "trailer"
    ) || name.starts_with(':')
}

/// Sends `req` through the proxy at `proxy`; HTTPS is verified against `roots` (Relay's CA).
pub async fn send_via_proxy(
    proxy: SocketAddr,
    roots: &[CertificateDer<'static>],
    req: &ComposeRequest,
) -> Result<ComposeResponse> {
    tokio::time::timeout(TIMEOUT, send_inner(proxy, roots, req))
        .await
        .map_err(|_| anyhow::anyhow!("no response after {} s", TIMEOUT.as_secs()))?
}

async fn send_inner(
    proxy: SocketAddr,
    roots: &[CertificateDer<'static>],
    req: &ComposeRequest,
) -> Result<ComposeResponse> {
    let method = Method::from_bytes(req.method.trim().to_ascii_uppercase().as_bytes())
        .context("bad method")?;
    let uri: Uri = req.url.trim().parse().context("bad URL")?;
    let host = uri.host().context("the URL needs a host")?.to_owned();
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => bail!("the URL must start with http:// or https://"),
    };
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let host_header = if uri.port_u16().is_some() {
        format!("{host}:{port}")
    } else {
        host.clone()
    };

    let mut stream = TcpStream::connect(proxy)
        .await
        .context("connecting to Relay's proxy")?;
    let (io, target): (Box<dyn Io>, String) = if https {
        stream
            .write_all(
                format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes(),
            )
            .await?;
        let head = crate::client::read_head(&mut stream).await?;
        if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
            bail!(
                "proxy refused CONNECT: {}",
                head.lines().next().unwrap_or("")
            );
        }
        let mut store = RootCertStore::empty();
        for r in roots {
            store.add(r.clone())?;
        }
        let cfg =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(store)
                .with_no_client_auth();
        let tls = TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from(host.clone())?, stream)
            .await
            .context("TLS handshake through proxy")?;
        let path = uri.path_and_query().map_or("/", |p| p.as_str()).to_owned();
        (Box::new(tls), path)
    } else {
        // Plain HTTP: absolute-form request line.
        (Box::new(stream), uri.to_string())
    };

    let started = Instant::now();
    let (mut sender, conn) = http1::handshake(TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = Request::builder().method(method.clone()).uri(target);
    for (k, v) in &req.headers {
        if is_framing(k) {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) else {
            continue; // a malformed header is dropped, not fatal
        };
        builder = builder.header(name, value);
    }
    builder = builder.header("host", host_header);
    if !req.body.is_empty() || matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        builder = builder.header("content-length", req.body.len());
    }
    let request = builder.body(Full::new(Bytes::from(req.body.clone())))?;

    let resp = sender.send_request(request).await?;
    let status = resp.status().as_u16();
    let headers_map: HeaderMap = resp.headers().clone();
    let headers = headers_map
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = resp.into_body().collect().await?.to_bytes();
    let duration_ms = started.elapsed().as_millis() as u64;
    let shown = &body[..body.len().min(MAX_PREVIEW)];
    Ok(ComposeResponse {
        status,
        headers,
        body_preview: crate::body::body_preview(&headers_map, shown, MAX_PREVIEW),
        body_size: body.len() as u64,
        duration_ms,
    })
}
