//! The traffic search language, shared by every UI, the store and the CLI.
//!
//! ```text
//! status:5xx host:api.acmeshop.com method:POST path:/v2/* duration:>800ms -state:tunneled "out of stock"
//! ```
//!
//! Terms are ANDed. `-term` negates. Values with spaces go in quotes. A bare word matches the URL,
//! headers or body previews (case-insensitive). Wildcards: `*` in host and path.

use std::fmt;

use crate::{Transaction, TransactionState};

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub terms: Vec<Term>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub negate: bool,
    pub kind: TermKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TermKind {
    /// Bare text: URL, headers or bodies contain it.
    Text(String),
    Status(StatusMatch),
    Method(String),
    Host(Glob),
    Path(Glob),
    Scheme(String),
    State(TransactionState),
    Decrypted(bool),
    /// "HTTP/1.1", "HTTP/2" (`h1`, `h2`, `http2` accepted).
    Version(String),
    /// Header name, optionally with a value substring (`header:x-app-version` / `header:x-env=staging`).
    Header {
        name: String,
        value: Option<String>,
    },
    /// Request or response body preview contains the text.
    Body(String),
    /// The program on this Mac that made the request (substring, e.g. `app:chrome`, `app:AcmeShop`).
    App(String),
    /// The client's address (prefix), e.g. `client:192.168.0.204` for a phone on Wi-Fi.
    Client(String),
    DurationMs(Cmp),
    /// Response size in bytes.
    Size(Cmp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMatch {
    Exact(u16),
    /// 1 = 1xx … 5xx.
    Class(u8),
    Cmp(Op, u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cmp {
    pub op: Op,
    pub value: u64,
}

/// A case-insensitive pattern where `*` matches any run of characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

impl Filter {
    pub fn parse(input: &str) -> Result<Filter, ParseError> {
        let mut terms = Vec::new();
        for raw in tokenize(input)? {
            let (negate, tok) = match raw.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_string()),
                _ => (false, raw),
            };
            terms.push(Term {
                negate,
                kind: parse_term(&tok)?,
            });
        }
        Ok(Filter { terms })
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn matches(&self, t: &Transaction) -> bool {
        self.terms
            .iter()
            .all(|term| term.kind.matches(t) != term.negate)
    }

    /// Plain-text needles (bare words and `body:`) a full-text index can use to narrow a search.
    pub fn text_needles(&self) -> Vec<&str> {
        self.terms
            .iter()
            .filter(|t| !t.negate)
            .filter_map(|t| match &t.kind {
                TermKind::Text(s) | TermKind::Body(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Splits on whitespace, keeping `"quoted values"` (also after `key:`) together.
fn tokenize(input: &str) -> Result<Vec<String>, ParseError> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in input.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_quotes {
        return Err(ParseError("unclosed quote".into()));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn parse_term(tok: &str) -> Result<TermKind, ParseError> {
    let Some((key, value)) = tok.split_once(':') else {
        return Ok(TermKind::Text(tok.to_lowercase()));
    };
    // `https://…` or `host:port`-looking bare text is still text.
    let key_l = key.to_ascii_lowercase();
    if value.starts_with("//") || value.is_empty() {
        return Ok(TermKind::Text(tok.to_lowercase()));
    }
    let err = |what: &str| ParseError(format!("{what} in `{tok}`"));
    Ok(match key_l.as_str() {
        "status" | "code" => TermKind::Status(
            parse_status(value).ok_or_else(|| err("expected a status like 200, 5xx or >=400"))?,
        ),
        "method" => TermKind::Method(value.to_ascii_uppercase()),
        "host" | "domain" => TermKind::Host(Glob(value.to_lowercase())),
        "path" | "url" => TermKind::Path(Glob(value.to_lowercase())),
        "scheme" => TermKind::Scheme(value.to_ascii_lowercase()),
        "state" | "is" => TermKind::State(
            parse_state(value).ok_or_else(|| err("expected complete, failed, tunneled or open"))?,
        ),
        "decrypted" | "ssl" => {
            TermKind::Decrypted(parse_bool(value).ok_or_else(|| err("expected true or false"))?)
        }
        "version" | "http" | "proto" => TermKind::Version(parse_version(value)),
        "header" | "h" => match value.split_once('=') {
            Some((n, v)) => TermKind::Header {
                name: n.to_ascii_lowercase(),
                value: Some(v.to_lowercase()),
            },
            None => TermKind::Header {
                name: value.to_ascii_lowercase(),
                value: None,
            },
        },
        "body" => TermKind::Body(value.to_lowercase()),
        "app" | "process" | "program" => TermKind::App(value.to_lowercase()),
        "client" | "ip" | "from" => TermKind::Client(value.to_lowercase()),
        "duration" | "time" => TermKind::DurationMs(
            parse_cmp(value, parse_ms)
                .ok_or_else(|| err("expected a duration like >800ms or <2s"))?,
        ),
        "size" => TermKind::Size(
            parse_cmp(value, parse_bytes)
                .ok_or_else(|| err("expected a size like >1MB or <10kb"))?,
        ),
        _ => {
            return Err(ParseError(format!(
                "unknown filter `{key}:` (try status, method, host, path, app, client, duration, size, body, header, state)"
            )));
        }
    })
}

fn split_op(v: &str) -> (Op, &str) {
    for (p, op) in [
        (">=", Op::Ge),
        ("<=", Op::Le),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Eq),
    ] {
        if let Some(rest) = v.strip_prefix(p) {
            return (op, rest);
        }
    }
    (Op::Eq, v)
}

fn parse_cmp(v: &str, unit: fn(&str) -> Option<u64>) -> Option<Cmp> {
    let (op, rest) = split_op(v);
    Some(Cmp {
        op,
        value: unit(rest)?,
    })
}

fn parse_status(v: &str) -> Option<StatusMatch> {
    let lower = v.to_ascii_lowercase();
    if lower.len() == 3 && lower.ends_with("xx") {
        let c = lower.as_bytes()[0];
        return (b'1'..=b'5')
            .contains(&c)
            .then(|| StatusMatch::Class(c - b'0'));
    }
    let (op, rest) = split_op(v);
    let n: u16 = rest.parse().ok()?;
    Some(if op == Op::Eq {
        StatusMatch::Exact(n)
    } else {
        StatusMatch::Cmp(op, n)
    })
}

fn parse_state(v: &str) -> Option<TransactionState> {
    Some(match v.to_ascii_lowercase().as_str() {
        "complete" | "ok" | "done" => TransactionState::Complete,
        "failed" | "fail" | "error" | "err" => TransactionState::Failed,
        "tunneled" | "tunnel" | "passthrough" => TransactionState::Tunneled,
        "open" | "inprogress" | "pending" | "active" => TransactionState::InProgress,
        _ => return None,
    })
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn parse_version(v: &str) -> String {
    match v.to_ascii_lowercase().as_str() {
        "h2" | "http2" | "2" | "http/2" | "h2c" => "HTTP/2".into(),
        "h1" | "http1" | "1.1" | "http/1.1" => "HTTP/1.1".into(),
        "h3" | "http3" | "3" | "http/3" => "HTTP/3".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// "800", "800ms", "1.5s", "2m".
fn parse_ms(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    let (num, mult) = if let Some(n) = v.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = v.strip_suffix('s') {
        (n, 1000.0)
    } else if let Some(n) = v.strip_suffix('m') {
        (n, 60_000.0)
    } else {
        (v.as_str(), 1.0)
    };
    let x: f64 = num.trim().parse().ok()?;
    (x >= 0.0).then(|| (x * mult).round() as u64)
}

/// "512", "10kb", "1.5MB", "2gb" (powers of 1024).
fn parse_bytes(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    let units = [
        ("gb", 1u64 << 30),
        ("mb", 1 << 20),
        ("kb", 1 << 10),
        ("g", 1 << 30),
        ("m", 1 << 20),
        ("k", 1 << 10),
        ("b", 1),
    ];
    let (num, mult) = units
        .iter()
        .find_map(|(u, m)| v.strip_suffix(u).map(|n| (n.to_string(), *m)))
        .unwrap_or((v.clone(), 1));
    let x: f64 = num.trim().parse().ok()?;
    (x >= 0.0).then(|| (x * mult as f64).round() as u64)
}

fn cmp(op: Op, a: u64, b: u64) -> bool {
    match op {
        Op::Lt => a < b,
        Op::Le => a <= b,
        Op::Gt => a > b,
        Op::Ge => a >= b,
        Op::Eq => a == b,
    }
}

impl Glob {
    pub fn new(p: &str) -> Self {
        Glob(p.to_lowercase())
    }

    /// `*` matches any run; without `*` the pattern must match the whole value, except that a
    /// host pattern also matches subdomains (handled by the caller) and a path pattern matches as a prefix.
    pub fn matches(&self, value: &str) -> bool {
        glob_match(self.0.as_bytes(), value.to_lowercase().as_bytes())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn glob_match(p: &[u8], v: &[u8]) -> bool {
    // Iterative wildcard match with backtracking on the last `*`.
    let (mut pi, mut vi, mut star, mut mark) = (0, 0, None, 0);
    while vi < v.len() {
        if pi < p.len() && (p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    hay.to_lowercase().contains(needle_lower)
}

impl TermKind {
    pub fn matches(&self, t: &Transaction) -> bool {
        match self {
            TermKind::Text(s) => {
                contains_ci(&t.url(), s)
                    || t.request_headers
                        .iter()
                        .chain(&t.response_headers)
                        .any(|h| contains_ci(&h.name, s) || contains_ci(&h.value, s))
                    || t.request_body_preview
                        .as_deref()
                        .is_some_and(|b| contains_ci(b, s))
                    || t.response_body_preview
                        .as_deref()
                        .is_some_and(|b| contains_ci(b, s))
                    || t.error.as_deref().is_some_and(|e| contains_ci(e, s))
            }
            TermKind::Status(m) => match (t.status, m) {
                (Some(s), StatusMatch::Exact(n)) => s == *n,
                (Some(s), StatusMatch::Class(c)) => s / 100 == *c as u16,
                (Some(s), StatusMatch::Cmp(op, n)) => cmp(*op, s as u64, *n as u64),
                (None, _) => false,
            },
            TermKind::Method(m) => t.method.eq_ignore_ascii_case(m),
            TermKind::Host(g) => {
                g.matches(&t.host)
                    || (!g.as_str().contains('*')
                        && t.host.to_lowercase().ends_with(&format!(".{}", g.as_str())))
            }
            TermKind::Path(g) => {
                let path = t.path.to_lowercase();
                g.matches(&path) || (!g.as_str().contains('*') && path.starts_with(g.as_str()))
            }
            TermKind::Scheme(s) => t.scheme.eq_ignore_ascii_case(s),
            TermKind::State(s) => t.state == *s,
            TermKind::Decrypted(d) => t.decrypted == *d,
            TermKind::Version(v) => {
                t.http_version.eq_ignore_ascii_case(v)
                    || t.upstream_http_version
                        .as_deref()
                        .is_some_and(|u| u.eq_ignore_ascii_case(v))
            }
            TermKind::Header { name, value } => t
                .request_headers
                .iter()
                .chain(&t.response_headers)
                .any(|h| {
                    h.name.eq_ignore_ascii_case(name)
                        && value.as_deref().is_none_or(|v| contains_ci(&h.value, v))
                }),
            TermKind::Body(s) => {
                t.request_body_preview
                    .as_deref()
                    .is_some_and(|b| contains_ci(b, s))
                    || t.response_body_preview
                        .as_deref()
                        .is_some_and(|b| contains_ci(b, s))
            }
            TermKind::App(a) => t
                .client_process
                .as_deref()
                .is_some_and(|p| p.to_lowercase().contains(a)),
            TermKind::Client(c) => t.client_addr.to_lowercase().starts_with(c),
            TermKind::DurationMs(c) => cmp(c.op, t.duration_ms, c.value),
            TermKind::Size(c) => cmp(c.op, t.response_body_size, c.value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Header;

    fn tx(method: &str, host: &str, path: &str, status: Option<u16>, ms: u64) -> Transaction {
        Transaction {
            id: 1,
            started_at_ms: 0,
            duration_ms: ms,
            method: method.into(),
            scheme: "https".into(),
            host: host.into(),
            port: 443,
            path: path.into(),
            status,
            state: TransactionState::Complete,
            error: None,
            decrypted: true,
            http_version: "HTTP/1.1".into(),
            upstream_http_version: Some("HTTP/2".into()),
            request_headers: vec![Header {
                name: "X-App-Version".into(),
                value: "3.4.1 (812)".into(),
            }],
            response_headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            request_body_size: 0,
            response_body_size: 2048,
            request_body_preview: None,
            response_body_preview: Some(r#"{"error":"PAYMENT_PROVIDER_TIMEOUT"}"#.into()),
            client_addr: "10.0.0.2:5000".into(),
            websocket_messages: vec![],
            client_process: None,
            applied_rules: vec![],
        }
    }

    fn m(q: &str, t: &Transaction) -> bool {
        Filter::parse(q).unwrap().matches(t)
    }

    #[test]
    fn the_brief_example_matches_the_failing_checkout() {
        let checkout = tx("POST", "api.acmeshop.com", "/v2/checkout", Some(500), 1840);
        assert!(m(
            "status:5xx host:api.acmeshop.com duration:>800ms",
            &checkout
        ));
        assert!(!m(
            "status:5xx host:api.acmeshop.com duration:>2s",
            &checkout
        ));
    }

    #[test]
    fn status_forms() {
        let t = tx("GET", "a.com", "/", Some(404), 10);
        assert!(
            m("status:404", &t)
                && m("status:4xx", &t)
                && m("status:>=400", &t)
                && m("code:<500", &t)
        );
        assert!(!m("status:2xx", &t) && !m("status:>404", &t));
        assert!(
            !m("status:2xx", &tx("CONNECT", "a.com", "", None, 0)),
            "no status never matches"
        );
    }

    #[test]
    fn host_and_path_wildcards_and_subdomains() {
        let t = tx(
            "GET",
            "api.acmeshop.com",
            "/v2/products/8231?page=2",
            Some(200),
            10,
        );
        assert!(m("host:acmeshop.com", &t), "subdomains match a bare domain");
        assert!(m("host:*.acmeshop.com", &t) && m("host:API.AcmeShop.com", &t));
        assert!(!m("host:acmeshop.co", &t) && !m("host:shop.com", &t));
        assert!(m("path:/v2/products", &t), "prefix");
        assert!(m("path:/v2/*/8231*", &t) && !m("path:/v3", &t));
    }

    #[test]
    fn text_header_body_and_negation() {
        let t = tx("POST", "api.acmeshop.com", "/v2/checkout", Some(500), 10);
        assert!(
            m("checkout", &t) && m("payment_provider", &t),
            "bare text searches URL and bodies"
        );
        assert!(m(r#"body:"PROVIDER_TIMEOUT""#, &t));
        assert!(
            m("header:x-app-version", &t)
                && m("header:x-app-version=3.4", &t)
                && !m("header:x-app-version=4.0", &t)
        );
        assert!(m("-method:GET", &t) && !m("-status:5xx", &t));
    }

    #[test]
    fn units_versions_states() {
        let t = tx("GET", "a.com", "/", Some(200), 1500);
        assert!(m("duration:>1s", &t) && m("duration:<=1.5s", &t) && !m("duration:<1500ms", &t));
        assert!(m("size:2kb", &t) && m("size:>1k", &t) && !m("size:>1MB", &t));
        assert!(m("version:h2", &t) && m("http:1.1", &t), "either leg");
        assert!(m("state:complete", &t) && m("decrypted:true", &t) && !m("is:failed", &t));
    }

    #[test]
    fn app_and_client_terms() {
        let mut t = tx("GET", "httpbin.org", "/json", Some(200), 10);
        t.client_process = Some("AcmeShop".into());
        assert!(m("app:acmeshop", &t) && m("app:shop", &t) && m("process:ACME", &t));
        assert!(!m("app:chrome", &t));
        assert!(
            !m("app:acme", &tx("GET", "a.com", "/", Some(200), 1)),
            "unknown process never matches"
        );
        assert!(m("client:10.0.0.2", &t) && !m("client:192.168", &t));
        assert!(m("-app:chrome host:httpbin.org", &t));
    }

    #[test]
    fn urls_typed_as_text_are_not_mistaken_for_keys() {
        let t = tx("GET", "api.acmeshop.com", "/v2/x", Some(200), 1);
        assert!(m("https://api.acmeshop.com/v2", &t));
    }

    #[test]
    fn helpful_errors() {
        assert!(
            Filter::parse("stauts:500")
                .unwrap_err()
                .0
                .contains("unknown filter")
        );
        assert!(
            Filter::parse("status:abc")
                .unwrap_err()
                .0
                .contains("status")
        );
        assert!(Filter::parse(r#"body:"unclosed"#).is_err());
        assert!(Filter::parse("").unwrap().is_empty());
    }

    #[test]
    fn text_needles_feed_the_index() {
        let f = Filter::parse(r#"checkout body:"out of stock" -timeout status:5xx"#).unwrap();
        assert_eq!(f.text_needles(), vec!["checkout", "out of stock"]);
    }
}
