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
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use relay_model::{Header, Transaction, TransactionState};
use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;

use crate::body::{body_preview, strip_hop_by_hop};
use crate::ca::{CertAuthority, SniResolver};
use crate::sniff;
use crate::tee::{ProxyBody, TeeBody, TeeOutcome, full};
use crate::{EventSink, ProxyConfig};

/// Magic host that serves the Relay CA certificate (open http://relay.ca on a device).
pub const CA_DOWNLOAD_HOST: &str = "relay.ca";

type UpstreamClient = Client<HttpsConnector<HttpConnector>, ProxyBody>;
/// Prior-knowledge HTTP/2 over plain TCP (h2c), for apps that speak h2c (e.g. gRPC in development).
type H2cClient = Client<HttpConnector, ProxyBody>;

pub(crate) struct Shared {
    pub ca: Arc<CertAuthority>,
    client: UpstreamClient,
    /// HTTP/1.1-only client for WebSocket upgrades (an upgrade can't happen over HTTP/2).
    ws_client: UpstreamClient,
    h2c: H2cClient,
    sink: Arc<dyn EventSink>,
    next_id: AtomicU64,
    cfg: ProxyConfig,
    /// Hosts that refused Relay's certificate this run; tunneled untouched from then on.
    learned_passthrough: std::sync::Mutex<std::collections::HashSet<String>>,
    procs: Arc<ProcCache>,
    listen_port: u16,
}

/// Client port → program, filled from `lsof` snapshots (see `procinfo`).
#[derive(Default)]
struct ProcCache {
    map: std::sync::Mutex<std::collections::HashMap<u16, String>>,
    /// Lets one lookup at a time run `lsof`; the others find the result in the cache afterwards.
    refresh: tokio::sync::Mutex<()>,
}

impl ProcCache {
    fn get(&self, port: u16) -> Option<String> {
        self.map.lock().unwrap().get(&port).cloned()
    }

    async fn resolve(&self, client_port: u16, relay_port: u16) {
        if self.get(client_port).is_some() {
            return;
        }
        let _one_at_a_time = self.refresh.lock().await;
        if self.get(client_port).is_some() {
            return; // an earlier lookup's snapshot already covered it
        }
        let found = tokio::task::spawn_blocking(move || crate::procinfo::snapshot(relay_port))
            .await
            .unwrap_or_default();
        let mut map = self.map.lock().unwrap();
        if map.len() > 8192 {
            map.clear(); // ports are reused; stale names must not outlive their connections
        }
        map.extend(found);
    }
}

/// Fills `client_process` from the cache. If the lookup has not finished when a transaction ends
/// (it takes a few tens of milliseconds, a fast request can finish first), the finished transaction
/// is sent again with the name once it is known. Consumers replace a row by its id.
struct ProcSink {
    inner: Arc<dyn EventSink>,
    procs: Arc<ProcCache>,
    relay_port: u16,
}

impl EventSink for ProcSink {
    fn on_transaction(&self, mut t: Transaction) {
        let loopback_port = (t.client_addr.starts_with("127.0.0.1:")
            || t.client_addr.starts_with("[::1]:"))
        .then(|| {
            t.client_addr
                .rsplit(':')
                .next()
                .and_then(|p| p.parse::<u16>().ok())
        })
        .flatten();
        let Some(port) = loopback_port.filter(|_| t.client_process.is_none()) else {
            return self.inner.on_transaction(t);
        };
        if let Some(name) = self.procs.get(port) {
            t.client_process = Some(name);
            return self.inner.on_transaction(t);
        }
        let finished = t.state != TransactionState::InProgress;
        self.inner.on_transaction(t.clone());
        if finished {
            let (procs, inner, relay_port) =
                (self.procs.clone(), self.inner.clone(), self.relay_port);
            tokio::spawn(async move {
                procs.resolve(port, relay_port).await;
                if let Some(name) = procs.get(port) {
                    t.client_process = Some(name);
                    inner.on_transaction(t);
                }
            });
        }
    }
}

impl Shared {
    fn is_passthrough(&self, name: &str) -> bool {
        self.cfg.is_passthrough(name)
            || self
                .learned_passthrough
                .lock()
                .unwrap()
                .contains(&name.trim_end_matches('.').to_ascii_lowercase())
    }
}

/// True when the client's handshake error says it rejected our certificate (not a network glitch).
fn certificate_rejected(err: &str) -> bool {
    [
        "CertificateUnknown",
        "UnknownCA",
        "BadCertificate",
        "CertificateExpired",
        "UnsupportedCertificate",
    ]
    .iter()
    .any(|a| err.contains(a))
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
        TransparentHandler {
            shared: self.shared.clone(),
        }
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
        tokio::spawn(async move {
            handle_tunnel(shared, stream, dst.ip().to_string(), dst.port(), src).await
        });
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub async fn start(cfg: ProxyConfig, sink: Arc<dyn EventSink>) -> Result<ProxyHandle> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let ca = Arc::new(CertAuthority::load_or_create(
        &cfg.data_dir,
        provider.clone(),
    )?);
    let client = build_upstream_client(provider.clone(), &cfg, true)?;
    let ws_client = build_upstream_client(provider, &cfg, false)?;
    let h2c = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(http_connector(&cfg));
    let listener = bind_with_fallback(cfg.listen, cfg.port_fallback).await?;
    let local_addr = listener.local_addr()?;
    let procs: Arc<ProcCache> = Arc::default();
    let sink: Arc<dyn EventSink> = if cfg.identify_client_process {
        Arc::new(ProcSink {
            inner: sink,
            procs: procs.clone(),
            relay_port: local_addr.port(),
        })
    } else {
        sink
    };
    let shared = Arc::new(Shared {
        ca,
        client,
        ws_client,
        h2c,
        sink,
        next_id: AtomicU64::new(1),
        cfg,
        learned_passthrough: Default::default(),
        procs,
        listen_port: local_addr.port(),
    });
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
    Ok(ProxyHandle {
        local_addr,
        shared,
        shutdown: Some(tx),
    })
}

fn http_connector(cfg: &ProxyConfig) -> HttpConnector {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http.set_connect_timeout(Some(cfg.connect_timeout));
    http
}

fn build_upstream_client(
    provider: Arc<CryptoProvider>,
    cfg: &ProxyConfig,
    allow_h2: bool,
) -> Result<UpstreamClient> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for r in &cfg.extra_upstream_roots {
        roots.add(r.clone()).context("adding extra upstream root")?;
    }
    let tls = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let builder = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1();
    let https = if allow_h2 {
        builder.enable_http2().wrap_connector(http_connector(cfg))
    } else {
        builder.wrap_connector(http_connector(cfg))
    };
    Ok(Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .build(https))
}

/// Binds `addr`; if its port is taken, tries the next `fallback` ports (port 0 = any free port).
async fn bind_with_fallback(addr: SocketAddr, fallback: u16) -> Result<TcpListener> {
    let mut last_err = None;
    let tries = if addr.port() == 0 { 0 } else { fallback };
    for offset in 0..=tries {
        let mut a = addr;
        a.set_port(addr.port().saturating_add(offset));
        match TcpListener::bind(a).await {
            Ok(l) => {
                if offset > 0 {
                    tracing::warn!(wanted = %addr, using = %a, "listen port busy, using the next free one");
                }
                return Ok(l);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow::anyhow!(
        "binding {addr} (+{tries} following ports): {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

async fn serve_client(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr) {
    if shared.cfg.identify_client_process && peer.ip().is_loopback() {
        // In the background: the name is attached when the transaction is recorded.
        let (procs, port, relay_port) = (shared.procs.clone(), peer.port(), shared.listen_port);
        tokio::spawn(async move { procs.resolve(port, relay_port).await });
    }
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

async fn handle_proxy_request(
    shared: Arc<Shared>,
    peer: SocketAddr,
    req: Request<Incoming>,
) -> Response<ProxyBody> {
    if req.method() == Method::CONNECT {
        let Some(authority) = req.uri().authority().cloned() else {
            return text_response(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
        };
        let host = authority.host().to_string();
        let port = authority.port_u16().unwrap_or(443);
        tokio::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    handle_tunnel(shared, TokioIo::new(upgraded), host, port, peer).await
                }
                Err(e) => tracing::debug!(error = %e, "CONNECT upgrade failed"),
            }
        });
        return Response::new(full(Bytes::new()));
    }

    let host = req
        .uri()
        .host()
        .map(str::to_owned)
        .or_else(|| host_header(req.headers()));
    if req.uri().authority().is_none() || host.as_deref() == Some(CA_DOWNLOAD_HOST) {
        // Origin-form request aimed at the proxy itself (a phone opening the setup page), or the
        // magic CA host (`http://relay.ca` → the certificate).
        let path = req.uri().path();
        if host.as_deref() == Some(CA_DOWNLOAD_HOST) && path == "/" {
            return ca_download_response(&shared);
        }
        let host_h = host.clone().unwrap_or_default();
        return match crate::setup::route(path, &host_h, shared.ca.ca_pem(), shared.ca.ca_der()) {
            Some(page) => {
                let mut r = Response::new(full(Bytes::from(page.body)));
                r.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(page.content_type),
                );
                if let Some(name) = page.filename
                    && let Ok(v) =
                        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
                {
                    r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
                }
                r
            }
            None => text_response(StatusCode::NOT_FOUND, "Relay: nothing here. Open /setup\n"),
        };
    }
    forward(&shared, req, "http", None, peer, false).await
}

/// Handles a byte stream whose destination is `host:port` (after CONNECT, or from the VPN):
/// decrypt TLS, decode plain HTTP, or relay raw bytes.
async fn handle_tunnel<S>(
    shared: Arc<Shared>,
    mut client: S,
    host: String,
    port: u16,
    peer: SocketAddr,
) where
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
    let passthrough = shared.is_passthrough(&name) || shared.is_passthrough(&host);
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
async fn serve_plain_http<S>(
    shared: Arc<Shared>,
    client: S,
    host: String,
    port: u16,
    peer: SocketAddr,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let fallback = if port == 80 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let svc = service_fn(move |req: Request<Incoming>| {
        let shared = shared.clone();
        let fallback = fallback.clone();
        async move {
            // Prefer the Host header (real name); fall back to the destination IP.
            let authority = host_header(req.headers()).unwrap_or(fallback);
            Ok::<_, Infallible>(forward(&shared, req, "http", Some(authority), peer, false).await)
        }
    });
    // HTTP/1.1 or h2c with prior knowledge, detected from the connection preface.
    let mut builder = AutoBuilder::new(TokioExecutor::new());
    builder.http1().preserve_header_case(true);
    if let Err(e) = builder
        .serve_connection_with_upgrades(TokioIo::new(client), svc)
        .await
    {
        tracing::debug!(%host, error = %e, "plain HTTP connection ended with error");
    }
}

async fn intercept_tls<S>(shared: Arc<Shared>, client: S, host: String, port: u16, peer: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let started = Instant::now();
    let started_ms = now_ms();
    let resolver = SniResolver {
        ca: shared.ca.clone(),
        fallback_host: host.clone(),
    };
    let mut server_cfg = match ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    {
        Ok(b) => b
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver)),
        Err(e) => {
            tracing::error!(error = %e, "TLS config");
            return;
        }
    };
    server_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

    let tls = match acceptor.accept(client).await {
        Ok(tls) => tls,
        Err(e) => {
            let mut t = base_transaction(
                &shared, "CONNECT", "https", &host, port, "", peer, started_ms,
            );
            t.state = TransactionState::Failed;
            t.duration_ms = started.elapsed().as_millis() as u64;
            let reason = e.to_string();
            let learned = shared.cfg.auto_passthrough && certificate_rejected(&reason);
            if learned {
                shared
                    .learned_passthrough
                    .lock()
                    .unwrap()
                    .insert(host.trim_end_matches('.').to_ascii_lowercase());
            }
            t.error = Some(format!(
                "TLS handshake with the client failed ({reason}). The app may pin certificates or not trust the Relay CA.{}",
                if learned {
                    " Relay passes this host through untouched from now on, so the app keeps working."
                } else {
                    ""
                }
            ));
            shared.sink.on_transaction(t);
            return;
        }
    };

    let sni = tls
        .get_ref()
        .1
        .server_name()
        .map(str::to_owned)
        .unwrap_or_else(|| host.clone());
    let authority = if port == 443 {
        sni.clone()
    } else {
        format!("{sni}:{port}")
    };
    let svc_shared = shared.clone();
    let svc = service_fn(move |req| {
        let shared = svc_shared.clone();
        let authority = authority.clone();
        async move {
            Ok::<_, Infallible>(forward(&shared, req, "https", Some(authority), peer, true).await)
        }
    });
    let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
    let result = if h2 {
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(tls), svc)
            .await
    } else {
        http1::Builder::new()
            .preserve_header_case(true)
            .serve_connection(TokioIo::new(tls), svc)
            .with_upgrades()
            .await
    };
    if let Err(e) = result {
        tracing::debug!(%host, h2, error = %e, "decrypted connection ended with error");
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
    let mut t = base_transaction(
        &shared, "CONNECT", scheme, &name, port, "", peer, started_ms,
    );
    let host = target;
    let connect = tokio::time::timeout(
        shared.cfg.connect_timeout,
        TcpStream::connect((host.as_str(), port)),
    );
    match connect
        .await
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")))
    {
        Ok(mut upstream) => {
            let _ = upstream.set_nodelay(true);
            let mut open = t.clone();
            open.state = TransactionState::InProgress;
            open.duration_ms = started.elapsed().as_millis() as u64;
            shared.sink.on_transaction(open);
            // Count on the app's side as bytes flow: copy_bidirectional drops its totals when either
            // side resets the connection, which many servers and apps do.
            let (read, written) = relay_tunnel(
                &mut client,
                &mut upstream,
                shared.cfg.tunnel_half_close_grace,
            )
            .await;
            t.state = TransactionState::Tunneled;
            t.request_body_size = read;
            t.response_body_size = written;
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
) -> Response<ProxyBody> {
    if is_websocket_upgrade(&req) {
        return forward_upgrade(shared, req, scheme, tls_authority, peer, decrypted).await;
    }
    let started = Instant::now();
    let started_ms = now_ms();
    let (parts, body) = req.into_parts();

    let authority = tls_authority
        .or_else(|| parts.uri.authority().map(|a| a.to_string()))
        .or_else(|| host_header(&parts.headers))
        .unwrap_or_default();
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".into());
    let (host, port) = split_authority(&authority, if scheme == "https" { 443 } else { 80 });
    let mut t = base_transaction(
        shared,
        parts.method.as_str(),
        scheme,
        &host,
        port,
        &path,
        peer,
        started_ms,
    );
    t.decrypted = decrypted;
    t.http_version = version_str(parts.version).to_owned();
    t.request_headers = to_headers(&parts.headers);

    // Stream the request upstream while keeping a bounded copy for the inspector.
    let (req_tee, req_done) = TeeBody::new(body, shared.cfg.max_preview_bytes);
    let req_tee = req_tee.with_expected_len(content_length(&parts.headers));

    let uri: Uri = match format!("{scheme}://{authority}{path}").parse() {
        Ok(u) => u,
        Err(e) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::BAD_REQUEST,
                format!("bad target URL: {e}"),
            );
        }
    };
    let mut out = Request::builder()
        .method(parts.method.clone())
        .uri(uri)
        .version(Version::HTTP_11);
    let mut headers = parts.headers.clone();
    strip_hop_by_hop(&mut headers);
    headers.remove(HeaderName::from_static("proxy-authorization"));
    // The target URI carries the authority. hyper's client writes `Host` from it on HTTP/1.1 and
    // `:authority` on HTTP/2; a copied `Host` on HTTP/2 is rejected by strict servers (Google's GFE).
    headers.remove(header::HOST);
    *out.headers_mut().unwrap() = headers;
    let out = match out.body(req_tee.boxed()) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::BAD_REQUEST,
                format!("building request: {e}"),
            );
        }
    };

    // An app speaking h2c can only be matched by an h2c upstream; everything else negotiates via ALPN.
    let upstream = if scheme == "http" && parts.version == Version::HTTP_2 {
        shared.h2c.request(out)
    } else {
        shared.client.request(out)
    };
    let resp = match tokio::time::timeout(shared.cfg.upstream_timeout, upstream).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::BAD_GATEWAY,
                format!("upstream error: {}", error_chain(&e)),
            );
        }
        Err(_) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::GATEWAY_TIMEOUT,
                "upstream timed out".into(),
            );
        }
    };
    let (rparts, rbody) = resp.into_parts();
    t.status = Some(rparts.status.as_u16());
    t.upstream_http_version = Some(version_str(rparts.version).to_owned());
    t.response_headers = to_headers(&rparts.headers);
    // Show the exchange immediately; the final event (same id) follows when the body ends.
    let mut open = t.clone();
    open.state = TransactionState::InProgress;
    open.duration_ms = started.elapsed().as_millis() as u64;
    shared.sink.on_transaction(open);

    // The response streams to the client as it arrives (downloads, SSE, long polls); the
    // transaction is recorded once both bodies have finished or been cut off.
    let (resp_tee, resp_done) = TeeBody::new(rbody, shared.cfg.max_preview_bytes);
    let resp_tee = resp_tee.with_expected_len(content_length(&rparts.headers));
    let sink = shared.sink.clone();
    let max = shared.cfg.max_preview_bytes;
    let (req_headers, resp_headers) = (parts.headers.clone(), rparts.headers.clone());
    tokio::spawn(async move {
        let req = req_done.await.unwrap_or_default();
        let resp = resp_done.await.unwrap_or_default();
        t.request_body_size = req.total;
        t.request_body_preview = preview(&req_headers, &req, max);
        t.response_body_size = resp.total;
        t.response_body_preview = preview(&resp_headers, &resp, max);
        t.duration_ms = started.elapsed().as_millis() as u64;
        match resp.error {
            Some(e) => {
                t.state = TransactionState::Failed;
                t.error = Some(format!(
                    "response interrupted after {} bytes: {e}",
                    resp.total
                ));
            }
            None => t.state = TransactionState::Complete,
        }
        sink.on_transaction(t);
    });

    let mut response = Response::new(resp_tee.boxed());
    *response.status_mut() = rparts.status;
    let mut rheaders = rparts.headers;
    strip_hop_by_hop(&mut rheaders);
    *response.headers_mut() = rheaders;
    response
}

fn is_websocket_upgrade<B>(req: &Request<B>) -> bool {
    let has_token = |name: header::HeaderName, token: &str| {
        req.headers()
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    };
    req.version() == Version::HTTP_11
        && has_token(header::CONNECTION, "upgrade")
        && has_token(header::UPGRADE, "websocket")
}

/// WebSocket: forward the upgrade over HTTP/1.1, then relay both directions and record frames.
async fn forward_upgrade(
    shared: &Shared,
    mut req: Request<Incoming>,
    scheme: &str,
    tls_authority: Option<String>,
    peer: SocketAddr,
    decrypted: bool,
) -> Response<ProxyBody> {
    let started = Instant::now();
    let started_ms = now_ms();
    let authority = tls_authority
        .or_else(|| req.uri().authority().map(|a| a.to_string()))
        .or_else(|| host_header(req.headers()))
        .unwrap_or_default();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".into());
    let (host, port) = split_authority(&authority, if scheme == "https" { 443 } else { 80 });
    let mut t = base_transaction(shared, "GET", scheme, &host, port, &path, peer, started_ms);
    t.decrypted = decrypted;
    t.http_version = version_str(req.version()).to_owned();
    t.request_headers = to_headers(req.headers());
    let client_upgrade = hyper::upgrade::on(&mut req);

    let uri: Uri = match format!("{scheme}://{authority}{path}").parse() {
        Ok(u) => u,
        Err(e) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::BAD_REQUEST,
                format!("bad target URL: {e}"),
            );
        }
    };
    let mut headers = req.headers().clone();
    for name in [
        "proxy-connection",
        "proxy-authorization",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    headers.remove(header::HOST);
    headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    let mut out = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .version(Version::HTTP_11);
    *out.headers_mut().unwrap() = headers;
    let out = match out.body(full(Bytes::new())) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::BAD_REQUEST,
                format!("building request: {e}"),
            );
        }
    };
    let mut resp = match tokio::time::timeout(
        shared.cfg.upstream_timeout,
        shared.ws_client.request(out),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::BAD_GATEWAY,
                format!("upstream error: {}", error_chain(&e)),
            );
        }
        Err(_) => {
            return fail(
                shared,
                t,
                started,
                StatusCode::GATEWAY_TIMEOUT,
                "upstream timed out".into(),
            );
        }
    };
    t.status = Some(resp.status().as_u16());
    t.upstream_http_version = Some(version_str(resp.version()).to_owned());
    t.response_headers = to_headers(resp.headers());

    if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        // The server declined the upgrade: pass its answer through as a normal response.
        t.state = TransactionState::Complete;
        t.duration_ms = started.elapsed().as_millis() as u64;
        shared.sink.on_transaction(t);
        let (rparts, rbody) = resp.into_parts();
        let mut r = Response::new(
            rbody
                .map_err(|e| Box::new(e) as crate::tee::BoxError)
                .boxed(),
        );
        *r.status_mut() = rparts.status;
        *r.headers_mut() = rparts.headers;
        return r;
    }

    let upstream_upgrade = hyper::upgrade::on(&mut resp);
    let mut open = t.clone();
    open.state = TransactionState::InProgress;
    open.duration_ms = started.elapsed().as_millis() as u64;
    shared.sink.on_transaction(open);

    let sink = shared.sink.clone();
    let grace = shared.cfg.tunnel_half_close_grace;
    tokio::spawn(async move {
        match (client_upgrade.await, upstream_upgrade.await) {
            (Ok(c), Ok(u)) => {
                let r = relay_websocket(TokioIo::new(c), TokioIo::new(u), grace).await;
                t.request_body_size = r.sent;
                t.response_body_size = r.received;
                t.websocket_messages = r.messages;
                if r.dropped > 0 {
                    t.error = Some(format!(
                        "{} more WebSocket messages were not kept",
                        r.dropped
                    ));
                }
                t.state = TransactionState::Complete;
            }
            (Err(e), _) | (_, Err(e)) => {
                t.state = TransactionState::Failed;
                t.error = Some(format!("WebSocket upgrade failed: {e}"));
            }
        }
        t.duration_ms = started.elapsed().as_millis() as u64;
        sink.on_transaction(t);
    });

    let mut response = Response::new(full(Bytes::new()));
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *response.headers_mut() = resp.headers().clone();
    response
}

struct WsRelayed {
    sent: u64,
    received: u64,
    messages: Vec<relay_model::WsMessage>,
    dropped: u64,
}

/// Relays an upgraded WebSocket connection, recording frames in both directions.
async fn relay_websocket<C, U>(client: C, upstream: U, grace: Duration) -> WsRelayed
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    use relay_model::WsDirection;
    use tokio::io::AsyncWriteExt;
    let rec = std::sync::Mutex::new(crate::ws::WsRecorder::default());
    let (sent, received) = (AtomicU64::new(0), AtomicU64::new(0));
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut ur, mut uw) = tokio::io::split(upstream);
    {
        let up = async {
            let mut buf = vec![0u8; 16 * 1024];
            while let Ok(n) = cr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                rec.lock().unwrap().feed(WsDirection::Send, &buf[..n]);
                sent.fetch_add(n as u64, Ordering::Relaxed);
                if uw.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            let _ = uw.shutdown().await;
        };
        let down = async {
            let mut buf = vec![0u8; 16 * 1024];
            while let Ok(n) = ur.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                rec.lock().unwrap().feed(WsDirection::Receive, &buf[..n]);
                received.fetch_add(n as u64, Ordering::Relaxed);
                if cw.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            let _ = cw.shutdown().await;
        };
        tokio::pin!(up, down);
        tokio::select! {
            _ = &mut up => { let _ = tokio::time::timeout(grace, &mut down).await; }
            _ = &mut down => { let _ = tokio::time::timeout(grace, &mut up).await; }
        }
    } // the copy futures (and their borrows of `rec`) end here
    let rec = rec.into_inner().unwrap();
    WsRelayed {
        sent: sent.load(Ordering::Relaxed),
        received: received.load(Ordering::Relaxed),
        messages: rec.messages,
        dropped: rec.dropped,
    }
}

fn preview(headers: &HeaderMap, o: &TeeOutcome, max: usize) -> Option<String> {
    let mut p = body_preview(headers, &o.captured, max)?;
    if o.truncated && !p.ends_with("[truncated]") {
        p.push_str(&format!(
            "\n…[truncated — first {} of {} bytes captured]",
            o.captured.len(),
            o.total
        ));
    }
    Some(p)
}

fn fail(
    shared: &Shared,
    mut t: Transaction,
    started: Instant,
    status: StatusCode,
    msg: String,
) -> Response<ProxyBody> {
    t.state = TransactionState::Failed;
    t.duration_ms = started.elapsed().as_millis() as u64;
    t.error = Some(msg.clone());
    shared.sink.on_transaction(t);
    text_response(status, &format!("Relay: {msg}\n"))
}

fn ca_download_response(shared: &Shared) -> Response<ProxyBody> {
    let mut r = Response::new(full(Bytes::from(shared.ca.ca_pem().to_owned())));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-x509-ca-cert"),
    );
    r.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"relay-ca.pem\""),
    );
    r
}

fn text_response(status: StatusCode, msg: &str) -> Response<ProxyBody> {
    let mut r = Response::new(full(Bytes::from(msg.to_owned())));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
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
        http_version: "HTTP/1.1".to_owned(),
        upstream_http_version: None,
        request_headers: vec![],
        response_headers: vec![],
        request_body_size: 0,
        response_body_size: 0,
        request_body_preview: None,
        response_body_preview: None,
        client_addr: peer.to_string(),
        websocket_messages: vec![],
        client_process: None,
    }
}

fn to_headers(map: &HeaderMap) -> Vec<Header> {
    map.iter()
        .map(|(k, v)| Header {
            name: k.as_str().to_owned(),
            value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
        })
        .collect()
}

fn host_header(h: &HeaderMap) -> Option<String> {
    h.get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn split_authority(authority: &str, default_port: u16) -> (String, u16) {
    match authority.parse::<http::uri::Authority>() {
        Ok(a) => (a.host().to_owned(), a.port_u16().unwrap_or(default_port)),
        Err(_) => (authority.to_owned(), default_port),
    }
}

fn content_length(h: &HeaderMap) -> Option<u64> {
    h.get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn version_str(v: Version) -> &'static str {
    match v {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/?",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// Copies both directions of a tunnel and returns (bytes from the app, bytes to the app).
///
/// Bytes are counted as they flow, so totals survive resets (copy_bidirectional drops them on
/// error). When one direction ends, the other gets `grace` to finish before both are closed.
async fn relay_tunnel<C, U>(client: &mut C, upstream: &mut U, grace: Duration) -> (u64, u64)
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let mut counted = Counted::new(client);
    let (read, written) = (counted.read.clone(), counted.written.clone());
    let (mut cr, mut cw) = tokio::io::split(&mut counted);
    let (mut ur, mut uw) = tokio::io::split(upstream);
    let up = async {
        let _ = tokio::io::copy(&mut cr, &mut uw).await;
        let _ = uw.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut ur, &mut cw).await;
        let _ = cw.shutdown().await;
    };
    tokio::pin!(up, down);
    tokio::select! {
        _ = &mut up => { let _ = tokio::time::timeout(grace, &mut down).await; }
        _ = &mut down => { let _ = tokio::time::timeout(grace, &mut up).await; }
    }
    (
        read.load(Ordering::Relaxed),
        written.load(Ordering::Relaxed),
    )
}

/// Counts bytes read from (app → server) and written to (server → app) the wrapped stream.
struct Counted<S> {
    inner: S,
    read: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
}

impl<S> Counted<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            read: Arc::default(),
            written: Arc::default(),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = r {
            self.read
                .fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, data);
        if let Poll::Ready(Ok(n)) = r {
            self.written.fetch_add(n as u64, Ordering::Relaxed);
        }
        r
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
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
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
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
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
