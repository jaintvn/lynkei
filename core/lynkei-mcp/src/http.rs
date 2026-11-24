//! MCP over HTTP ("streamable HTTP", JSON replies): `POST /mcp` with a JSON-RPC message.
//!
//! The Lynkei app runs this on a fixed local port, so any agent that can use a URL connects the same
//! way, once: `{"mcpServers": {"lynkei": {"url": "http://127.0.0.1:8890/mcp"}}}`.
//!
//! It only listens on loopback and refuses requests whose `Host` or `Origin` is not local, so a web
//! page in your browser cannot talk to it.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::Ctx;

const MAX_BODY: usize = 8 * 1024 * 1024;

pub async fn serve(addr: &str, ctx: Arc<Ctx>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    eprintln!("lynkei-mcp: listening on http://{addr}/mcp");
    loop {
        let (stream, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(300), connection(stream, ctx))
                    .await;
        });
    }
}

struct Request {
    method: String,
    path: String,
    host: String,
    origin: Option<String>,
    body: Vec<u8>,
    keep_alive: bool,
}

async fn read_request(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Option<Request> {
    // Read until the end of the headers.
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let (mut host, mut origin, mut len, mut keep_alive) = (String::new(), None, 0usize, true);
    for l in lines {
        let (k, v) = l.split_once(':')?;
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "host" => host = v.to_string(),
            "origin" => origin = Some(v.to_string()),
            "content-length" => len = v.parse().ok()?,
            "connection" => keep_alive = !v.eq_ignore_ascii_case("close"),
            _ => {}
        }
    }
    if len > MAX_BODY {
        return None;
    }
    let body_start = head_end + 4;
    while buf.len() < body_start + len {
        let mut chunk = [0u8; 8192];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[body_start..body_start + len].to_vec();
    buf.drain(..body_start + len);
    Some(Request {
        method,
        path,
        host,
        origin,
        body,
        keep_alive,
    })
}

fn local_host(h: &str) -> bool {
    let name = h.rsplit_once(':').map(|(n, _)| n).unwrap_or(h);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn local_origin(o: &str) -> bool {
    let rest = o
        .strip_prefix("http://")
        .or_else(|| o.strip_prefix("https://"))
        .unwrap_or(o);
    local_host(rest)
}

async fn respond(
    stream: &mut TcpStream,
    status: &str,
    body: &str,
    keep_alive: bool,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: {}\r\n\r\n",
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await
}

async fn connection(mut stream: TcpStream, ctx: Arc<Ctx>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    while let Some(req) = read_request(&mut stream, &mut buf).await {
        let ka = req.keep_alive;
        if !local_host(&req.host) || req.origin.as_deref().is_some_and(|o| !local_origin(o)) {
            respond(
                &mut stream,
                "403 Forbidden",
                r#"{"error":"Lynkei only answers local requests"}"#,
                false,
            )
            .await?;
            return Ok(());
        }
        let path = req.path.split('?').next().unwrap_or("");
        match (req.method.as_str(), path) {
            ("GET", "/") | ("GET", "/health") => {
                let body =
                    json!({"name": "lynkei", "mcp": "/mcp", "version": env!("CARGO_PKG_VERSION")})
                        .to_string();
                respond(&mut stream, "200 OK", &body, ka).await?;
            }
            ("POST", "/mcp") => {
                let reply = match serde_json::from_slice::<Value>(&req.body) {
                    Ok(Value::Array(batch)) => {
                        let mut out = Vec::new();
                        for m in &batch {
                            if let Some(r) = crate::handle(m, &ctx).await {
                                out.push(r);
                            }
                        }
                        (!out.is_empty()).then_some(Value::Array(out))
                    }
                    Ok(msg) => crate::handle(&msg, &ctx).await,
                    Err(e) => Some(
                        json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
                    ),
                };
                match reply {
                    Some(r) => respond(&mut stream, "200 OK", &r.to_string(), ka).await?,
                    None => {
                        stream
                            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n")
                            .await?
                    }
                }
            }
            (_, "/mcp") => {
                respond(
                    &mut stream,
                    "405 Method Not Allowed",
                    r#"{"error":"POST JSON-RPC messages to /mcp"}"#,
                    ka,
                )
                .await?;
            }
            _ => respond(&mut stream, "404 Not Found", r#"{"error":"not found"}"#, ka).await?,
        }
        if !ka {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_hosts_and_origins_are_accepted() {
        assert!(local_host("127.0.0.1:8890"));
        assert!(local_host("localhost:8890"));
        assert!(!local_host("evil.example"));
        assert!(!local_host("127.0.0.1.evil.example:8890"));
        assert!(local_origin("http://localhost:3000"));
        assert!(!local_origin("https://evil.example"));
    }
}
