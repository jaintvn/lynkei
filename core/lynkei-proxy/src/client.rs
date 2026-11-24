//! A tiny HTTP(S)-through-proxy client. Used by tests and `lynkei-cli fetch` to prove
//! that traffic is decrypted end to end (it trusts only the roots it is given).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http::{Request, Uri};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

pub struct FetchResult {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// GETs `url` through the proxy at `proxy`. HTTPS is verified against `roots` only.
pub async fn fetch_via_proxy(
    proxy: SocketAddr,
    url: &str,
    roots: &[CertificateDer<'static>],
) -> Result<FetchResult> {
    let uri: Uri = url.parse().context("parsing URL")?;
    let host = uri.host().context("URL needs a host")?.to_owned();
    let https = uri.scheme_str() == Some("https");
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let mut stream = TcpStream::connect(proxy)
        .await
        .context("connecting to proxy")?;

    if https {
        let connect = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
        stream.write_all(connect.as_bytes()).await?;
        let head = read_head(&mut stream).await?;
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
        let path = uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
            .to_owned();
        send(tls, &host, port, &path).await
    } else {
        // Plain HTTP: absolute-form request to the proxy.
        send(stream, &host, port, url).await
    }
}

/// GETs `url` directly (no proxy). HTTPS is verified against `roots`, or the public web PKI if `None`.
/// Used to test VPN capture: the VPN, not a proxy setting, is what routes this through Lynkei.
pub async fn fetch_direct(
    url: &str,
    roots: Option<&[CertificateDer<'static>]>,
) -> Result<FetchResult> {
    let uri: Uri = url.parse().context("parsing URL")?;
    let host = uri.host().context("URL needs a host")?.to_owned();
    let https = uri.scheme_str() == Some("https");
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .context("connecting")?;
    let path = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_owned();
    if !https {
        return send(stream, &host, port, &path).await;
    }
    let mut store = RootCertStore::empty();
    match roots {
        Some(r) => {
            for c in r {
                store.add(c.clone())?;
            }
        }
        None => store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let cfg =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(store)
            .with_no_client_auth();
    let tls = TlsConnector::from(Arc::new(cfg))
        .connect(ServerName::try_from(host.clone())?, stream)
        .await
        .context("TLS handshake")?;
    send(tls, &host, port, &path).await
}

async fn send<S>(io: S, host: &str, port: u16, target: &str) -> Result<FetchResult>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = http1::handshake(TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let host_header = if port == 443 || port == 80 {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    let req = Request::get(target)
        .header("host", host_header)
        .header("user-agent", "lynkei-cli/0.1")
        .header("accept-encoding", "gzip")
        .body(Empty::<Bytes>::new())?;
    let resp = sender.send_request(req).await?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = resp.into_body().collect().await?.to_bytes();
    Ok(FetchResult {
        status,
        headers,
        body,
    })
}

pub(crate) async fn read_head(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            bail!("proxy closed the connection");
        }
        buf.push(byte[0]);
        if buf.len() > 16 * 1024 {
            bail!("proxy response head too large");
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
