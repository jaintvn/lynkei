//! A tiny local API for the Acme Shop demo apps, so the demos need no internet.
//!
//! It answers the same paths the demos used to call on httpbin.org (`/get`, `/json`, `/post`,
//! `/status/N`, `/delay/N`, `/image/png`) plus `/zen`. Run it with `scripts/demo-server.sh`, or
//! `lynkei-demo-server [port]` (default 8099, listens on every interface so a phone can reach it).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 8099;
/// `/delay/N` waits at most this long, like httpbin.
const MAX_DELAY_SECS: u64 = 10;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = match std::env::args().nth(1) {
        Some(p) => p.parse::<u16>()?,
        None => DEFAULT_PORT,
    };
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    println!("Acme Shop mock API on http://0.0.0.0:{port}  (Ctrl-C to stop)");
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let _ = http1::Builder::new()
                        .serve_connection(io, service_fn(handle))
                        .await;
                });
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let reply = route(&parts.method, &parts.uri, &parts.headers, &body).await;
    println!(
        "{} {} -> {}",
        parts.method,
        parts.uri,
        reply.status().as_u16()
    );
    Ok(reply)
}

fn reply(status: u16, content_type: &str, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(body.into()));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    r.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("static content type"),
    );
    r
}

fn json_reply(status: u16, value: &Value) -> Response<Full<Bytes>> {
    reply(
        status,
        "application/json",
        serde_json::to_vec_pretty(value).unwrap_or_default(),
    )
}

async fn route(
    method: &Method,
    uri: &hyper::Uri,
    headers: &hyper::HeaderMap,
    body: &[u8],
) -> Response<Full<Bytes>> {
    let path = uri.path();
    let echo = || echo(method, uri, headers, body);
    match path {
        "/get" if method == Method::GET => json_reply(200, &echo()),
        "/post" if method == Method::POST => json_reply(200, &echo()),
        "/json" => json_reply(200, &catalog()),
        "/zen" => reply(
            200,
            "text/plain; charset=utf-8",
            "Keep it logically awesome.\n",
        ),
        "/image/png" => reply(200, "image/png", demo_png()),
        _ => {
            if let Some(code) = path.strip_prefix("/status/") {
                return match code.parse::<u16>() {
                    Ok(c) if (100..600).contains(&c) => status_reply(c),
                    _ => reply(400, "text/plain", "bad status code\n"),
                };
            }
            if let Some(secs) = path.strip_prefix("/delay/") {
                let secs = secs.parse::<u64>().unwrap_or(1).min(MAX_DELAY_SECS);
                tokio::time::sleep(Duration::from_secs(secs)).await;
                return json_reply(200, &echo());
            }
            json_reply(404, &json!({ "error": "not found", "path": path }))
        }
    }
}

/// The checkout and token calls answer with a believable JSON error body, not just a status.
fn status_reply(code: u16) -> Response<Full<Bytes>> {
    match code {
        401 => json_reply(
            401,
            &json!({ "error": "token_expired", "message": "Refresh token has expired" }),
        ),
        500 => json_reply(
            500,
            &json!({ "error": "checkout_failed", "message": "Payment service unavailable" }),
        ),
        _ => reply(code, "text/plain", ""),
    }
}

/// Mirrors httpbin's shape: what the server saw of the request.
fn echo(method: &Method, uri: &hyper::Uri, headers: &hyper::HeaderMap, body: &[u8]) -> Value {
    let mut args = Map::new();
    for pair in uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        args.insert(k.to_owned(), Value::String(v.to_owned()));
    }
    let mut seen = Map::new();
    for (name, value) in headers {
        seen.insert(
            name.as_str().to_owned(),
            Value::String(String::from_utf8_lossy(value.as_bytes()).into_owned()),
        );
    }
    let text = String::from_utf8_lossy(body).into_owned();
    json!({
        "method": method.as_str(),
        "url": uri.to_string(),
        "args": args,
        "headers": seen,
        "data": text,
        "json": serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
    })
}

fn catalog() -> Value {
    json!({
        "products": [
            { "sku": "p_8231", "name": "Trail Runner 2", "category": "shoes", "price": 129.0, "stock": 14 },
            { "sku": "p_8232", "name": "City Sneaker", "category": "shoes", "price": 89.0, "stock": 0 },
            { "sku": "p_9114", "name": "Merino Socks (3 pack)", "category": "socks", "price": 24.5, "stock": 120 }
        ],
        "page": 1,
        "total": 3
    })
}

/// A 160×160 gradient PNG, built on the fly (zlib via flate2) so no binary file is checked in.
fn demo_png() -> Vec<u8> {
    use flate2::{Compression, Crc, write::ZlibEncoder};
    use std::io::Write;

    const SIZE: u32 = 160;
    let mut raw = Vec::with_capacity((SIZE * (SIZE * 3 + 1)) as usize);
    for y in 0..SIZE {
        raw.push(0); // filter: none
        for x in 0..SIZE {
            raw.extend_from_slice(&[255, (x * 255 / SIZE) as u8, (80 + y * 150 / SIZE) as u8]);
        }
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&raw).expect("in-memory write");
    let idat = z.finish().expect("in-memory finish");

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&SIZE.to_be_bytes());
    ihdr.extend_from_slice(&SIZE.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routes_answer_like_httpbin() {
        let h = hyper::HeaderMap::new();
        let get = |p: &str| p.parse::<hyper::Uri>().unwrap();

        let r = route(&Method::GET, &get("/get?category=shoes&page=2"), &h, b"").await;
        assert_eq!(r.status(), 200);
        let v: Value =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["args"]["category"], "shoes");

        let r = route(&Method::POST, &get("/post"), &h, br#"{"sku":"p_8231"}"#).await;
        let v: Value =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["json"]["sku"], "p_8231");

        assert_eq!(
            route(&Method::POST, &get("/status/500"), &h, b"")
                .await
                .status(),
            500
        );
        assert_eq!(
            route(&Method::POST, &get("/status/401"), &h, b"")
                .await
                .status(),
            401
        );
        assert_eq!(
            route(&Method::GET, &get("/nope"), &h, b"").await.status(),
            404
        );
        assert_eq!(
            route(&Method::GET, &get("/json"), &h, b"").await.status(),
            200
        );
    }

    #[test]
    fn png_has_signature_and_end_chunk() {
        let png = demo_png();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
