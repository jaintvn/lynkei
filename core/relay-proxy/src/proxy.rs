//! HTTP proxy server: plain HTTP forwarding, CONNECT tunnels, and TLS interception.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Request, Response, StatusCode, Uri, Version};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use relay_model::{Header, Transaction, TransactionState};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;

use crate::body::{body_preview, strip_hop_by_hop};
use crate::ca::{CertAuthority, SniResolver};
use crate::sniff;
use crate::{EventSink, ProxyConfig};

/// Magic host that serves the Relay CA certificate (open http://relay.ca on a device).
pub const CA_DOWNLOAD_HOST: &str = "relay.ca";

type UpstreamClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

pub(crate) struct Shared {
    pub ca: Arc<CertAuthority>,
    client: UpstreamClient,
    sink: Arc<dyn EventSink>,
    next_id: AtomicU64,
    cfg: ProxyConfig,
}

/// A running proxy. Dropping it (or calling [`ProxyHandle::shutdown`]) stops accepting connections.
pub struct ProxyHandle {
    pub local_addr: SocketAddr,
    shared: Arc<Shared>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl ProxyHandle {
    pub fn ca_pem(&self) -> &str {
        self.shared.ca.ca_pem()
    }
    pub fn ca_path(&self) -> &std::path::Path {
        self.shared.ca.ca_path()
    }
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }

    /// A cloneable entry point for connections that did not come through the proxy listener
    /// (VPN / transparent capture), sharing this proxy's CA, upstream pool and event sink.
    pub fn transparent(&self) -> TransparentHandler {
        TransparentHandler { shared: self.shared.clone() }
    }
}

#[derive(Clone)]
pub struct TransparentHandler {
    shared: Arc<Shared>,
}

impl TransparentHandler {
    /// Handles a connection from `src` that was headed to `dst`: TLS is intercepted (hostname from SNI),
    /// plain HTTP is decoded, anything else is relayed to `dst` untouched.
    pub fn handle<S>(&self, stream: S, dst: SocketAddr, src: SocketAddr)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let shared = self.shared.clone();
        tokio::spawn(async move { handle_tunnel(shared, stream, dst.ip().to_string(), dst.port(), src).await });
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub async fn start(cfg: ProxyConfig, sink: Arc<dyn EventSink>) -> Result<ProxyHandle> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let ca = Arc::new(CertAuthority::load_or_create(&cfg.data_dir, provider.clone())?);
    let client = build_upstream_client(provider, &cfg.extra_upstream_roots)?;
    let listener = TcpListener::bind(cfg.listen).await.with_context(|| format!("binding {}", cfg.listen))?;
    let local_addr = listener.local_addr()?;
    let shared = Arc::new(Shared { ca, client, sink, next_id: AtomicU64::new(1), cfg });
    let (tx, mut rx) = oneshot::channel();

    let accept_shared = shared.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let _ = stream.set_nodelay(true);
                        let shared = accept_shared.clone();
                        tokio::spawn(async move { serve_client(shared, stream, peer).await });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
        tracing::info!("proxy listener stopped");
    });

    tracing::info!(%local_addr, "proxy listening");
    Ok(ProxyHandle { local_addr, shared, shutdown: Some(tx) })
}

fn build_upstream_client(provider: Arc<CryptoProvider>, extra_roots: &[CertificateDer<'static>]) -> Result<UpstreamClient> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for r in extra_roots {
        roots.add(r.clone()).context("adding extra upstream root")?;
    }
    let tls = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .build();
    Ok(Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .build(https))
}

async fn serve_client(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr) {
    let svc = service_fn(move |req| {
        let shared = shared.clone();
        async move { Ok::<_, Infallible>(handle_proxy_request(shared, peer, req).await) }
    });
    if let Err(e) = http1::Builder::new()
        .preserve_header_case(true)
        .serve_connection(TokioIo::new(stream), svc)
        .with_upgrades()
        .await
    {
        tracing::debug!(%peer, error = %e, "client connection ended with error");
    }
}

async fn handle_proxy_request(shared: Arc<Shared>, peer: SocketAddr, req: Request<Incoming>) -> Response<Full<Bytes>> {
    if req.method() == Method::CONNECT {
        let Some(authority) = req.uri().authority().cloned() else {
            return text_response(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
        };
        let host = authority.host().to_string();
        let port = authority.port_u16().unwrap_or(443);
        tokio::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => handle_tunnel(shared, TokioIo::new(upgraded), host, port, peer).await,
                Err(e) => tracing::debug!(error = %e, "CONNECT upgrade failed"),
            }
        });
        return Response::new(Full::new(Bytes::new()));
    }

    let host = req.uri().host().map(str::to_owned).or_else(|| host_header(req.headers()));
    if req.uri().authority().is_none() || host.as_deref() == Some(CA_DOWNLOAD_HOST) {
        // Origin-form request aimed at the proxy itself, or the magic CA host.
        return ca_download_response(&shared);
    }
    forward(&shared, req, "http", None, peer, false).await
}

/// Handles a byte stream whose destination is `host:port` (after CONNECT, or from the VPN):
/// decrypt TLS, decode plain HTTP, or relay raw bytes.
async fn handle_tunnel<S>(shared: Arc<Shared>, mut client: S, host: String, port: u16, peer: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let first = match read_first_flight(&mut client).await {
        Some(b) if !b.is_empty() => b,
        _ => return,
    };
    let sni = sniff::parse_sni(&first);
    let is_tls = sniff::tls_record_len(&first).is_some();
    let is_http = !is_tls && sniff::looks_like_http(&first);
    // Through the VPN `host` is an IP address; the real name comes from SNI.
    let name = sni.clone().unwrap_or_else(|| host.clone());
    let passthrough = shared.cfg.is_passthrough(&name) || shared.cfg.is_passthrough(&host);
    let client = Prefixed::new(Bytes::from(first), client);

    if is_tls && !passthrough {
        intercept_tls(shared, client, name, port, peer).await;
    } else if is_http && !passthrough {
        serve_plain_http(shared, client, host, port, peer).await;
    } else {
        raw_tunnel(shared, client, name, host, port, peer, passthrough).await;
    }
}

/// Reads the client's first bytes; for TLS, keeps reading until the whole ClientHello record is in.
async fn read_first_flight<S: AsyncRead + Unpin>(client: &mut S) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let deadline = Duration::from_secs(30);
    loop {
        let n = match tokio::time::timeout(deadline, client.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            _ => return if buf.is_empty() { None } else { Some(buf) },
        };
        if n == 0 {
            return Some(buf);
        }
        buf.extend_from_slice(&chunk[..n]);
        match sniff::tls_record_len(&buf) {
            Some(need) if buf.len() < need && buf.len() < 16 * 1024 + 5 => continue,
            _ => return Some(buf),
        }
    }
}

/// Plain HTTP on a transparent connection: decode it like an absolute-form proxy request.
async fn serve_plain_http<S>(shared: Arc<Shared>, client: S, host: String, port: u16, peer: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let fallback = if port == 80 { host.clone() } else { format!("{host}:{port}") };
    let svc = service_fn(move |req: Request<Incoming>| {
        let shared = shared.clone();
        let fallback = fallback.clone();
        async move {
            // Prefer the Host header (real name); fall back to the destination IP.
            let authority = host_header(req.headers()).unwrap_or(fallback);
            Ok::<_, Infallible>(forward(&shared, req, "http", Some(authority), peer, false).await)
        }
    });
    if let Err(e) = http1::Builder::new().preserve_header_case(true).serve_connection(TokioIo::new(client), svc).await {
        tracing::debug!(%host, error = %e, "plain HTTP connection ended with error");
    }
}

async fn intercept_tls<S>(shared: Arc<Shared>, client: S, host: String, port: u16, peer: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let started = Instant::now();
    let started_ms = now_ms();
    let resolver = SniResolver { ca: shared.ca.clone(), fallback_host: host.clone() };
    let mut server_cfg = match ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
    {
        Ok(b) => b.with_no_client_auth().with_cert_resolver(Arc::new(resolver)),
        Err(e) => {
            tracing::error!(error = %e, "TLS config");
            return;
        }
    };
    // P0: HTTP/1.1 only on the client leg. HTTP/2 arrives in P1.
    server_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

    let tls = match acceptor.accept(client).await {
        Ok(tls) => tls,
        Err(e) => {
            let mut t = base_transaction(&shared, "CONNECT", "https", &host, port, "", peer, started_ms);
            t.state = TransactionState::Failed;
            t.duration_ms = started.elapsed().as_millis() as u64;
            t.error = Some(format!(
                "TLS handshake with the client failed ({e}). The app may pin certificates or not trust the Relay CA."
            ));
            shared.sink.on_transaction(t);
            return;
        }
    };

    let sni = tls.get_ref().1.server_name().map(str::to_owned).unwrap_or_else(|| host.clone());
    let authority = if port == 443 { sni.clone() } else { format!("{sni}:{port}") };
    let svc_shared = shared.clone();
    let svc = service_fn(move |req| {
        let shared = svc_shared.clone();
        let authority = authority.clone();
        async move { Ok::<_, Infallible>(forward(&shared, req, "https", Some(authority), peer, true).await) }
    });
    if let Err(e) = http1::Builder::new().preserve_header_case(true).serve_connection(TokioIo::new(tls), svc).await {
        tracing::debug!(%host, error = %e, "decrypted connection ended with error");
    }
}

/// Relays bytes untouched. `name` is what we show (SNI when known); `target` is where we connect.
async fn raw_tunnel<S>(
    shared: Arc<Shared>,
    mut client: S,
    name: String,
    target: String,
    port: u16,
    peer: SocketAddr,
    passthrough: bool,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let started = Instant::now();
    let started_ms = now_ms();
    let scheme = if port == 80 { "http" } else { "https" };
    let mut t = base_transaction(&shared, "CONNECT", scheme, &name, port, "", peer, started_ms);
    let host = target;
    match TcpStream::connect((host.as_str(), port)).await {
        Ok(mut upstream) => {
            let _ = upstream.set_nodelay(true);
            let (up, down) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await.unwrap_or((0, 0));
            t.state = TransactionState::Tunneled;
            t.request_body_size = up;
            t.response_body_size = down;
            if passthrough {
                t.error = Some("SSL passthrough host — not decrypted".into());
            }
        }
        Err(e) => {
            t.state = TransactionState::Failed;
            t.error = Some(format!("connecting to {host}:{port} failed: {e}"));
        }
    }
    t.duration_ms = started.elapsed().as_millis() as u64;
    shared.sink.on_transaction(t);
}

/// Sends `req` upstream, records the exchange, and returns the response for the client.
async fn forward(
    shared: &Shared,
    req: Request<Incoming>,
    scheme: &str,
    tls_authority: Option<String>,
    peer: SocketAddr,
    decrypted: bool,
) -> Response<Full<Bytes>> {
    let started = Instant::now();
    let started_ms = now_ms();
    let (parts, body) = req.into_parts();

    let authority = tls_authority
        .or_else(|| parts.uri.authority().map(|a| a.to_string()))
        .or_else(|| host_header(&parts.headers))
        .unwrap_or_default();
    let path = parts.uri.path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_else(|| "/".into());
    let (host, port) = split_authority(&authority, if scheme == "https" { 443 } else { 80 });
    let mut t = base_transaction(shared, parts.method.as_str(), scheme, &host, port, &path, peer, started_ms);
    t.decrypted = decrypted;
    t.request_headers = to_headers(&parts.headers);

    let req_body = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return fail(shared, t, started, StatusCode::BAD_REQUEST, format!("reading request body: {e}")),
    };
    t.request_body_size = req_body.len() as u64;
    t.request_body_preview = body_preview(&parts.headers, &req_body, shared.cfg.max_preview_bytes);

    let uri: Uri = match format!("{scheme}://{authority}{path}").parse() {
        Ok(u) => u,
        Err(e) => return fail(shared, t, started, StatusCode::BAD_REQUEST, format!("bad target URL: {e}")),
    };
    let mut out = Request::builder().method(parts.method.clone()).uri(uri).version(Version::HTTP_11);
    let mut headers = parts.headers.clone();
    strip_hop_by_hop(&mut headers);
    headers.remove(HeaderName::from_static("proxy-authorization"));
    if let Ok(v) = HeaderValue::from_str(&authority) {
        headers.insert(header::HOST, v);
    }
    *out.headers_mut().unwrap() = headers;
    let out = match out.body(Full::new(req_body)) {
        Ok(r) => r,
        Err(e) => return fail(shared, t, started, StatusCode::BAD_REQUEST, format!("building request: {e}")),
    };

    let resp = match tokio::time::timeout(shared.cfg.upstream_timeout, shared.client.request(out)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return fail(shared, t, started, StatusCode::BAD_GATEWAY, format!("upstream error: {}", error_chain(&e))),
        Err(_) => return fail(shared, t, started, StatusCode::GATEWAY_TIMEOUT, "upstream timed out".into()),
    };
    let (rparts, rbody) = resp.into_parts();
    let resp_body = match rbody.collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return fail(shared, t, started, StatusCode::BAD_GATEWAY, format!("reading upstream body: {e}")),
    };

    t.status = Some(rparts.status.as_u16());
    t.response_headers = to_headers(&rparts.headers);
    t.response_body_size = resp_body.len() as u64;
    t.response_body_preview = body_preview(&rparts.headers, &resp_body, shared.cfg.max_preview_bytes);
    t.state = TransactionState::Complete;
    t.duration_ms = started.elapsed().as_millis() as u64;
    shared.sink.on_transaction(t);

    let mut response = Response::new(Full::new(resp_body));
    *response.status_mut() = rparts.status;
    let mut rheaders = rparts.headers;
    strip_hop_by_hop(&mut rheaders);
    *response.headers_mut() = rheaders;
    response
}

fn fail(shared: &Shared, mut t: Transaction, started: Instant, status: StatusCode, msg: String) -> Response<Full<Bytes>> {
    t.state = TransactionState::Failed;
    t.duration_ms = started.elapsed().as_millis() as u64;
    t.error = Some(msg.clone());
    shared.sink.on_transaction(t);
    text_response(status, &format!("Relay: {msg}\n"))
}

fn ca_download_response(shared: &Shared) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(shared.ca.ca_pem().to_owned())));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-x509-ca-cert"));
    r.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"relay-ca.pem\""),
    );
    r
}

fn text_response(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(msg.to_owned())));
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    r
}

#[allow(clippy::too_many_arguments)]
fn base_transaction(
    shared: &Shared,
    method: &str,
    scheme: &str,
    host: &str,
    port: u16,
    path: &str,
    peer: SocketAddr,
    started_ms: u64,
) -> Transaction {
    Transaction {
        id: shared.next_id.fetch_add(1, Ordering::Relaxed),
        started_at_ms: started_ms,
        duration_ms: 0,
        method: method.to_owned(),
        scheme: scheme.to_owned(),
        host: host.to_owned(),
        port,
        path: path.to_owned(),
        status: None,
        state: TransactionState::Failed,
        error: None,
        decrypted: false,
        request_headers: vec![],
        response_headers: vec![],
        request_body_size: 0,
        response_body_size: 0,
        request_body_preview: None,
        response_body_preview: None,
        client_addr: peer.to_string(),
    }
}

fn to_headers(map: &HeaderMap) -> Vec<Header> {
    map.iter()
        .map(|(k, v)| Header { name: k.as_str().to_owned(), value: String::from_utf8_lossy(v.as_bytes()).into_owned() })
        .collect()
}

fn host_header(h: &HeaderMap) -> Option<String> {
    h.get(header::HOST).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

fn split_authority(authority: &str, default_port: u16) -> (String, u16) {
    match authority.parse::<http::uri::Authority>() {
        Ok(a) => (a.host().to_owned(), a.port_u16().unwrap_or(default_port)),
        Err(_) => (authority.to_owned(), default_port),
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

/// A stream that replays already-read bytes before reading from the inner stream.
pub(crate) struct Prefixed<S> {
    prefix: Bytes,
    inner: S,
}

impl<S> Prefixed<S> {
    pub(crate) fn new(prefix: Bytes, inner: S) -> Self {
        Self { prefix, inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.remaining());
            let chunk = self.prefix.split_to(n);
            buf.put_slice(&chunk);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
