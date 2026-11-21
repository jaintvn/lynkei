//! Header hygiene and human-readable body previews.

use std::io::Read;

use http::header::{self, HeaderMap, HeaderName};

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Removes hop-by-hop headers (RFC 9110 §7.6.1), including any listed in `Connection`.
pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let listed: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in listed {
        headers.remove(name);
    }
    // `te: trailers` is the one TE value allowed end to end (HTTP/2, gRPC depends on it).
    let keep_te = headers
        .get(header::TE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("trailers"));
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    if keep_te {
        headers.insert(header::TE, http::HeaderValue::from_static("trailers"));
    }
}

/// Returns a UTF-8 preview of a textual body, decompressing gzip/deflate. `None` for binary bodies.
pub fn body_preview(headers: &HeaderMap, body: &[u8], max_bytes: usize) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !content_type.is_empty() && !is_textual(content_type) {
        return None;
    }
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let decoded: Vec<u8> = match encoding.as_str() {
        "" | "identity" => body.to_vec(),
        "gzip" | "x-gzip" => inflate(flate2::read::GzDecoder::new(body), max_bytes)?,
        "deflate" => inflate(flate2::read::ZlibDecoder::new(body), max_bytes)
            .or_else(|| inflate(flate2::read::DeflateDecoder::new(body), max_bytes))?,
        _ => return Some(format!("[{encoding}-encoded body, {} bytes — decoding arrives in P1]", body.len())),
    };
    if content_type.is_empty() && std::str::from_utf8(&decoded[..decoded.len().min(512)]).is_err() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&decoded[..decoded.len().min(max_bytes)]).into_owned();
    if decoded.len() > max_bytes {
        text.push_str("\n…[truncated]");
    }
    Some(text)
}

/// Decompresses up to `max_bytes`. Keeps whatever decoded before an error, because streamed bodies
/// are captured only up to a limit and the compressed tail may be missing.
fn inflate(mut r: impl Read, max_bytes: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let _ = r.by_ref().take(max_bytes as u64 + 1).read_to_end(&mut out);
    (!out.is_empty()).then_some(out)
}

fn is_textual(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("text/")
        || ["json", "xml", "javascript", "x-www-form-urlencoded", "graphql", "yaml", "csv"]
            .iter()
            .any(|k| ct.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use std::io::Write;

    #[test]
    fn strips_hop_by_hop_and_connection_listed() {
        let mut h = HeaderMap::new();
        h.insert("connection", HeaderValue::from_static("keep-alive, x-secret"));
        h.insert("x-secret", HeaderValue::from_static("1"));
        h.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        h.insert("content-type", HeaderValue::from_static("application/json"));
        strip_hop_by_hop(&mut h);
        assert_eq!(h.len(), 1);
        assert!(h.contains_key("content-type"));
    }

    #[test]
    fn keeps_te_trailers_for_grpc() {
        let mut h = HeaderMap::new();
        h.insert("te", HeaderValue::from_static("trailers"));
        strip_hop_by_hop(&mut h);
        assert_eq!(h.get("te").unwrap(), "trailers");
        let mut g = HeaderMap::new();
        g.insert("te", HeaderValue::from_static("gzip"));
        strip_hop_by_hop(&mut g);
        assert!(g.get("te").is_none());
    }

    #[test]
    fn previews_gzip_json_and_skips_images() {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(br#"{"ok":true}"#).unwrap();
        let gz = enc.finish().unwrap();
        let mut h = HeaderMap::new();
        h.insert("content-type", HeaderValue::from_static("application/json; charset=utf-8"));
        h.insert("content-encoding", HeaderValue::from_static("gzip"));
        assert_eq!(body_preview(&h, &gz, 1024).as_deref(), Some(r#"{"ok":true}"#));

        let mut img = HeaderMap::new();
        img.insert("content-type", HeaderValue::from_static("image/webp"));
        assert_eq!(body_preview(&img, b"RIFF....", 1024), None);
    }

    #[test]
    fn truncates_long_bodies() {
        let mut h = HeaderMap::new();
        h.insert("content-type", HeaderValue::from_static("text/plain"));
        let p = body_preview(&h, &[b'a'; 100], 10).unwrap();
        assert!(p.starts_with("aaaaaaaaaa") && p.ends_with("[truncated]"));
    }
}
