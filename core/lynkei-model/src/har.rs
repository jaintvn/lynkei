//! HAR 1.2 export and import (the format browsers, Charles and most proxy tools share).
//!
//! Lynkei keeps only a capped text preview of each body, so exported `content.text` and
//! `postData.text` hold that preview (marked in `comment` when the body was larger), and binary
//! bodies are omitted. Tunnelled (not decrypted) connections have no HTTP exchange and are skipped.

use serde_json::{Value, json};

use crate::{Header, Transaction, TransactionState};

/// Exports finished transactions as a HAR document (pretty JSON).
pub fn export(transactions: &[Transaction]) -> String {
    let entries: Vec<Value> = transactions
        .iter()
        .filter(|t| t.state != TransactionState::Tunneled)
        .map(entry)
        .collect();
    let doc = json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "Lynkei", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries,
        }
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn headers_json(headers: &[Header]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|h| json!({ "name": h.name, "value": h.value }))
            .collect(),
    )
}

fn header<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn query_string(path: &str) -> Value {
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    Value::Array(
        query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                json!({ "name": k, "value": v })
            })
            .collect(),
    )
}

fn entry(t: &Transaction) -> Value {
    let req_type = header(&t.request_headers, "content-type").unwrap_or("");
    let resp_type = header(&t.response_headers, "content-type").unwrap_or("");
    let mut request = json!({
        "method": t.method,
        "url": t.url(),
        "httpVersion": t.http_version,
        "cookies": [],
        "headers": headers_json(&t.request_headers),
        "queryString": query_string(&t.path),
        "headersSize": -1,
        "bodySize": t.request_body_size,
    });
    if t.request_body_size > 0 {
        let mut post = json!({ "mimeType": req_type });
        if let Some(text) = &t.request_body_preview {
            post["text"] = json!(text);
            if (text.len() as u64) < t.request_body_size {
                post["comment"] = json!("truncated by Lynkei");
            }
        }
        request["postData"] = post;
    }
    let mut content = json!({ "size": t.response_body_size, "mimeType": resp_type });
    if let Some(text) = &t.response_body_preview {
        content["text"] = json!(text);
        if (text.len() as u64) < t.response_body_size {
            content["comment"] = json!("truncated by Lynkei");
        }
    }
    let response = json!({
        "status": t.status.unwrap_or(0),
        "statusText": t.status.map(reason).unwrap_or(""),
        "httpVersion": t.upstream_http_version.as_deref().unwrap_or(&t.http_version),
        "cookies": [],
        "headers": headers_json(&t.response_headers),
        "content": content,
        "redirectURL": header(&t.response_headers, "location").unwrap_or(""),
        "headersSize": -1,
        "bodySize": t.response_body_size,
    });
    let mut e = json!({
        "startedDateTime": iso8601(t.started_at_ms),
        "time": t.duration_ms,
        "request": request,
        "response": response,
        "cache": {},
        "timings": { "send": 0, "wait": t.duration_ms, "receive": 0 },
    });
    if let Some(app) = &t.client_process {
        e["_clientProcess"] = json!(app);
    }
    if let Some(err) = &t.error {
        e["_error"] = json!(err);
    }
    e
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-11-23T14:03:09.482Z`
fn iso8601(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` to epoch milliseconds.
fn parse_iso8601(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    let (clock, offset_min) = if let Some(c) = time.strip_suffix('Z') {
        (c, 0)
    } else if let Some(i) = time.rfind(['+', '-']).filter(|&i| i > 0) {
        let (c, off) = time.split_at(i);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (h, m) = off[1..].split_once(':').unwrap_or((&off[1..], "0"));
        (
            c,
            sign * (h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?),
        )
    } else {
        (time, 0)
    };
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, "0"));
    let mut p = hms.split(':');
    let (h, mi, sec) = (
        p.next()?.parse::<i64>().ok()?,
        p.next()?.parse::<i64>().ok()?,
        p.next()?.parse::<i64>().ok()?,
    );
    let millis = format!("{frac:0<3}")[..3].parse::<i64>().ok()?;
    let secs = days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    u64::try_from(secs * 1000 + millis).ok()
}

fn read_headers(v: &Value) -> Vec<Header> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    Some(Header {
                        name: h.get("name")?.as_str()?.to_owned(),
                        value: h.get("value")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reads a HAR document into transactions numbered from `first_id`.
pub fn import(text: &str, first_id: u64) -> Result<Vec<Transaction>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let entries = doc
        .pointer("/log/entries")
        .and_then(Value::as_array)
        .ok_or("not a HAR file (no log.entries)")?;
    let mut out = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let n = i + 1;
        let req = e.get("request").ok_or(format!("entry {n}: no request"))?;
        let url = req
            .get("url")
            .and_then(Value::as_str)
            .ok_or(format!("entry {n}: no request url"))?;
        let (scheme, rest) = url
            .split_once("://")
            .ok_or(format!("entry {n}: bad url {url}"))?;
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'?' => (&rest[..i], format!("/{}", &rest[i..])),
            Some(i) => (&rest[..i], rest[i..].to_owned()),
            None => (rest, "/".to_owned()),
        };
        let default_port = if scheme == "https" { 443 } else { 80 };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse().unwrap_or(default_port)),
            _ => (authority, default_port),
        };
        let resp = e.get("response");
        let status = resp
            .and_then(|r| r.get("status"))
            .and_then(Value::as_u64)
            .filter(|s| *s > 0)
            .map(|s| s as u16);
        let response_headers = resp
            .and_then(|r| r.get("headers"))
            .map(read_headers)
            .unwrap_or_default();
        let size =
            |v: Option<&Value>| v.and_then(Value::as_i64).filter(|n| *n >= 0).unwrap_or(0) as u64;
        let text_of = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
        let content = resp.and_then(|r| r.get("content"));
        let post = req.get("postData");
        let failed = status.is_none() || e.get("_error").is_some();
        out.push(Transaction {
            id: first_id + i as u64,
            started_at_ms: e
                .get("startedDateTime")
                .and_then(Value::as_str)
                .and_then(parse_iso8601)
                .unwrap_or(0),
            duration_ms: e
                .get("time")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .max(0.0) as u64,
            method: req
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_owned(),
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port,
            path,
            status,
            state: if failed {
                TransactionState::Failed
            } else {
                TransactionState::Complete
            },
            error: e.get("_error").and_then(Value::as_str).map(str::to_owned),
            decrypted: scheme == "https",
            http_version: req
                .get("httpVersion")
                .and_then(Value::as_str)
                .unwrap_or("HTTP/1.1")
                .to_owned(),
            upstream_http_version: None,
            request_headers: req.get("headers").map(read_headers).unwrap_or_default(),
            response_headers,
            request_body_size: size(req.get("bodySize")).max(
                post.and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                    .map_or(0, |t| t.len() as u64),
            ),
            response_body_size: size(content.and_then(|c| c.get("size")))
                .max(size(resp.and_then(|r| r.get("bodySize")))),
            request_body_preview: text_of(post.and_then(|p| p.get("text"))),
            response_body_preview: text_of(content.and_then(|c| c.get("text"))),
            client_addr: "imported".to_owned(),
            websocket_messages: vec![],
            client_process: e
                .get("_clientProcess")
                .and_then(Value::as_str)
                .map(str::to_owned),
            applied_rules: vec![],
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Transaction {
        Transaction {
            id: 7,
            started_at_ms: 1_795_443_789_482,
            duration_ms: 120,
            method: "POST".into(),
            scheme: "https".into(),
            host: "api.acmeshop.com".into(),
            port: 443,
            path: "/v2/cart?x=1&y=two".into(),
            status: Some(500),
            state: TransactionState::Complete,
            error: None,
            decrypted: true,
            http_version: "HTTP/2".into(),
            upstream_http_version: Some("HTTP/2".into()),
            request_headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            response_headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            request_body_size: 9,
            response_body_size: 14,
            request_body_preview: Some(r#"{"a":"b"}"#.into()),
            response_body_preview: Some(r#"{"error":"x"}"#.into()),
            client_addr: "127.0.0.1:5000".into(),
            websocket_messages: vec![],
            client_process: Some("AcmeShop".into()),
            applied_rules: vec![],
        }
    }

    #[test]
    fn timestamps_round_trip() {
        for ms in [0, 1_795_443_789_482, 951_782_400_000, 4_102_444_799_999] {
            assert_eq!(parse_iso8601(&iso8601(ms)), Some(ms));
        }
        assert_eq!(
            parse_iso8601("2026-11-23T10:00:00+05:30"),
            parse_iso8601("2026-11-23T04:30:00Z")
        );
    }

    #[test]
    fn export_then_import_keeps_the_exchange() {
        let har = export(&[sample()]);
        let doc: Value = serde_json::from_str(&har).unwrap();
        assert_eq!(doc["log"]["version"], "1.2");
        assert_eq!(
            doc["log"]["entries"][0]["request"]["queryString"][1]["value"],
            "two"
        );
        assert_eq!(doc["log"]["entries"][0]["response"]["status"], 500);

        let back = import(&har, 100).unwrap();
        assert_eq!(back.len(), 1);
        let t = &back[0];
        assert_eq!(t.id, 100);
        assert_eq!(t.url(), "https://api.acmeshop.com/v2/cart?x=1&y=two");
        assert_eq!(t.status, Some(500));
        assert_eq!(t.started_at_ms, 1_795_443_789_482);
        assert_eq!(t.request_body_preview.as_deref(), Some(r#"{"a":"b"}"#));
        assert_eq!(t.response_body_preview.as_deref(), Some(r#"{"error":"x"}"#));
        assert_eq!(t.client_process.as_deref(), Some("AcmeShop"));
    }

    #[test]
    fn tunnels_are_skipped_and_junk_is_rejected() {
        let mut tunnel = sample();
        tunnel.state = TransactionState::Tunneled;
        let doc: Value = serde_json::from_str(&export(&[tunnel])).unwrap();
        assert_eq!(doc["log"]["entries"].as_array().unwrap().len(), 0);
        assert!(import("{}", 1).is_err());
        assert!(import("nope", 1).is_err());
    }

    #[test]
    fn browser_style_entries_import() {
        let har = r#"{"log":{"entries":[{"startedDateTime":"2026-01-02T03:04:05.5+00:00","time":12.7,
            "request":{"method":"GET","url":"http://example.com:8080?q=1","httpVersion":"h2","headers":[],"bodySize":0},
            "response":{"status":0,"headers":[],"content":{"size":-1},"bodySize":-1}}]}}"#;
        let t = &import(har, 1).unwrap()[0];
        assert_eq!(
            (t.host.as_str(), t.port, t.path.as_str()),
            ("example.com", 8080, "/?q=1")
        );
        assert_eq!(t.state, TransactionState::Failed);
        assert_eq!(t.duration_ms, 12);
    }
}
