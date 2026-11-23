//! relay-proxy: the Relay capture engine (HTTP proxy + TLS interception).
//!
//! Platform-neutral: runs on macOS (inside relayd / the app) and on Android.

pub mod body;
pub mod breakpoints;
pub mod ca;
pub mod client;
pub mod compose;
pub mod procinfo;
mod proxy;
pub mod rules;
pub mod setup;
pub mod sniff;
pub mod tee;
pub mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

pub use proxy::{CA_DOWNLOAD_HOST, ProxyHandle, TransparentHandler, start};
pub use relay_model::{Header, Transaction, TransactionState, WsDirection, WsKind, WsMessage};
pub use rustls::pki_types::CertificateDer;

/// Receives exchanges as they progress. The same `id` may be delivered more than once — first as
/// `InProgress`, then with its final state — and later deliveries replace earlier ones.
/// Called from engine threads: implementations must not block.
pub trait EventSink: Send + Sync + 'static {
    fn on_transaction(&self, transaction: Transaction);
}

impl<F: Fn(Transaction) + Send + Sync + 'static> EventSink for F {
    fn on_transaction(&self, transaction: Transaction) {
        self(transaction)
    }
}

#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub listen: SocketAddr,
    /// Where the CA certificate and key live.
    pub data_dir: PathBuf,
    /// Hosts that are never decrypted (suffix match, e.g. "apple.com").
    pub passthrough_hosts: Vec<String>,
    pub max_preview_bytes: usize,
    pub upstream_timeout: Duration,
    /// Time allowed to open a TCP connection to a server.
    pub connect_timeout: Duration,
    /// If the listen port is busy, try up to this many following ports.
    pub port_fallback: u16,
    /// After one side of a tunnel finishes, how long the other side may keep going before both are
    /// closed. Some servers (Apple's) only close at their idle timeout, which would leave the
    /// tunnel "open" long after the app left.
    pub tunnel_half_close_grace: Duration,
    /// When an app refuses Relay's certificate (pinning, or it ignores user-installed CAs, which
    /// is most Android apps), pass that host through untouched from then on, so the app keeps
    /// working. The first refused connection still fails; the app's retry then succeeds.
    pub auto_passthrough: bool,
    /// Name the program on this Mac behind each loopback connection (macOS only).
    pub identify_client_process: bool,
    /// Extra trusted roots for upstream servers (tests, corporate CAs).
    pub extra_upstream_roots: Vec<CertificateDer<'static>>,
}

impl ProxyConfig {
    pub fn new(listen: SocketAddr, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            listen,
            data_dir: data_dir.into(),
            passthrough_hosts: default_passthrough(),
            max_preview_bytes: 256 * 1024,
            upstream_timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(10),
            port_fallback: 10,
            tunnel_half_close_grace: Duration::from_secs(5),
            auto_passthrough: true,
            identify_client_process: cfg!(target_os = "macos"),
            extra_upstream_roots: vec![],
        }
    }

    pub fn is_passthrough(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.passthrough_hosts
            .iter()
            .any(|p| host == *p || host.ends_with(&format!(".{p}")))
    }
}

/// Hosts that break (push, sign-in, OS services) when intercepted. Editable by the user later.
///
/// The Apple entries mirror the base domains in Apple's system-wide pinning list
/// (`/System/Library/Security/Certificates.bundle/Contents/Resources/CertificatePinning.plist`):
/// iOS/macOS reject any non-Apple certificate for them — even in Safari with a trusted root — so
/// decrypting them only produces errors. P1: read that plist at runtime on macOS.
pub fn default_passthrough() -> Vec<String> {
    [
        "apple.com",
        "apple.com.cn",
        "apple.cn",
        "icloud.com",
        "cdn-apple.com",
        "mzstatic.com",
        "itunes.com",
        "apple-cloudkit.com",
        "apple-studies.com",
        "apple-programs.com",
        "shazam.com",
        "push-apple.com.akadns.net",
        "mtalk.google.com",
        "android.clients.google.com",
        "play.googleapis.com",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_matches_suffix_only_on_label_boundary() {
        let cfg = ProxyConfig::new("127.0.0.1:0".parse().unwrap(), "/tmp");
        assert!(cfg.is_passthrough("api.push.apple.com"));
        assert!(
            cfg.is_passthrough("www.apple.com"),
            "Apple pins apple.com system-wide"
        );
        assert!(cfg.is_passthrough("icloud.com."));
        assert!(!cfg.is_passthrough("httpbin.org"));
        assert!(!cfg.is_passthrough("notpush.apple.com.evil.test"));
        assert!(!cfg.is_passthrough("myicloud.com"));
    }
}
