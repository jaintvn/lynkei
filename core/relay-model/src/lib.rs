//! Shared data model for Relay. Pure data: no I/O, no platform code.

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
    pub request_headers: Vec<Header>,
    pub response_headers: Vec<Header>,
    pub request_body_size: u64,
    pub response_body_size: u64,
    /// UTF-8 preview of textual bodies (decompressed), capped in size.
    pub request_body_preview: Option<String>,
    pub response_body_preview: Option<String>,
    pub client_addr: String,
}

impl Transaction {
    pub fn url(&self) -> String {
        let default_port = matches!((self.scheme.as_str(), self.port), ("http", 80) | ("https", 443));
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
            request_headers: vec![],
            response_headers: vec![Header { name: "Content-Type".into(), value: "application/json".into() }],
            request_body_size: 0,
            response_body_size: 0,
            request_body_preview: None,
            response_body_preview: None,
            client_addr: "127.0.0.1:5000".into(),
        }
    }

    #[test]
    fn url_omits_default_ports() {
        assert_eq!(tx("https", 443).url(), "https://api.acmeshop.com/v2/products?page=2");
        assert_eq!(tx("http", 8080).url(), "http://api.acmeshop.com:8080/v2/products?page=2");
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        assert_eq!(tx("https", 443).response_header("content-type"), Some("application/json"));
    }
}
