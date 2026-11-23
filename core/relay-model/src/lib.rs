//! Shared data model for Relay. Pure data: no I/O, no platform code.

pub mod filter;
pub mod rules;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionState {
    /// Request and response were seen in full (plain HTTP or decrypted HTTPS).
    Complete,
    /// The exchange failed (upstream error, TLS handshake rejected by the client, ...).
    Failed,
    /// Bytes were relayed without decryption (non-TLS CONNECT or a passthrough host).
    Tunneled,
    /// Still open: response headers received (body streaming) or tunnel connected. A later event
    /// with the same `id` replaces this one.
    InProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WsDirection {
    /// App → server.
    Send,
    /// Server → app.
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WsKind {
    Text,
    Binary,
    Continuation,
    Close,
    Ping,
    Pong,
    Other,
}

/// One WebSocket frame seen on an upgraded connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsMessage {
    pub direction: WsDirection,
    pub kind: WsKind,
    /// Payload size in bytes.
    pub size: u64,
    /// Text (or close reason) preview, capped; `None` for binary frames.
    pub preview: Option<String>,
    /// Milliseconds since the connection was upgraded.
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: u64,
    /// Unix epoch milliseconds when the request started.
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub method: String,
    /// "http" or "https".
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Path plus query string, e.g. `/v2/products?page=2`.
    pub path: String,
    pub status: Option<u16>,
    pub state: TransactionState,
    pub error: Option<String>,
    /// True when HTTPS traffic was decrypted by Relay.
    pub decrypted: bool,
    /// Protocol between the app and Relay, e.g. "HTTP/2".
    #[serde(default = "default_version")]
    pub http_version: String,
    /// Protocol between Relay and the server, once a response arrived.
    #[serde(default)]
    pub upstream_http_version: Option<String>,
    pub request_headers: Vec<Header>,
    pub response_headers: Vec<Header>,
    pub request_body_size: u64,
    pub response_body_size: u64,
    /// UTF-8 preview of textual bodies (decompressed), capped in size.
    pub request_body_preview: Option<String>,
    pub response_body_preview: Option<String>,
    pub client_addr: String,
    /// Frames of an upgraded WebSocket connection (capped per connection).
    #[serde(default)]
    pub websocket_messages: Vec<WsMessage>,
    /// The program on this Mac that opened the connection ("Google Chrome", "AcmeShop"), when known.
    /// Only available for connections from this Mac (including iOS Simulator apps).
    #[serde(default)]
    pub client_process: Option<String>,
    /// Ids of the rules that changed or answered this exchange.
    #[serde(default)]
    pub applied_rules: Vec<String>,
}

fn default_version() -> String {
    "HTTP/1.1".to_owned()
}

impl Transaction {
    pub fn url(&self) -> String {
        let default_port = matches!(
            (self.scheme.as_str(), self.port),
            ("http", 80) | ("https", 443)
        );
        if default_port {
            format!("{}://{}{}", self.scheme, self.host, self.path)
        } else {
            format!("{}://{}:{}{}", self.scheme, self.host, self.port, self.path)
        }
    }

    pub fn response_header(&self, name: &str) -> Option<&str> {
        self.response_headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(scheme: &str, port: u16) -> Transaction {
        Transaction {
            id: 1,
            started_at_ms: 0,
            duration_ms: 0,
            method: "GET".into(),
            scheme: scheme.into(),
            host: "api.acmeshop.com".into(),
            port,
            path: "/v2/products?page=2".into(),
            status: Some(200),
            state: TransactionState::Complete,
            error: None,
            decrypted: true,
            http_version: "HTTP/2".into(),
            upstream_http_version: Some("HTTP/2".into()),
            request_headers: vec![],
            response_headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            request_body_size: 0,
            response_body_size: 0,
            request_body_preview: None,
            response_body_preview: None,
            client_addr: "127.0.0.1:5000".into(),
            websocket_messages: vec![],
            client_process: None,
            applied_rules: vec![],
        }
    }

    #[test]
    fn url_omits_default_ports() {
        assert_eq!(
            tx("https", 443).url(),
            "https://api.acmeshop.com/v2/products?page=2"
        );
        assert_eq!(
            tx("http", 8080).url(),
            "http://api.acmeshop.com:8080/v2/products?page=2"
        );
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        assert_eq!(
            tx("https", 443).response_header("content-type"),
            Some("application/json")
        );
    }
}
