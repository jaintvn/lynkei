//! relay-ffi: the one boundary between the Rust core and the native apps (Swift now, Kotlin later).
//!
//! Keep this surface small: commands in, events out. Bodies will be fetched lazily by id in P1.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use relay_proxy::{EventSink, ProxyConfig, ProxyHandle, Transaction, TransactionState};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RelayError {
    #[error("{reason}")]
    Failed { reason: String },
}

impl From<anyhow::Error> for RelayError {
    fn from(e: anyhow::Error) -> Self {
        RelayError::Failed { reason: format!("{e:#}") }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiState {
    Complete,
    Failed,
    Tunneled,
    /// Still open; a later event with the same id replaces it.
    InProgress,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTransaction {
    pub id: u64,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub method: String,
    pub url: String,
    pub scheme: String,
    pub host: String,
    pub path: String,
    pub status: Option<u16>,
    pub state: FfiState,
    pub error: Option<String>,
    pub decrypted: bool,
    pub request_headers: Vec<FfiHeader>,
    pub response_headers: Vec<FfiHeader>,
    pub request_body_size: u64,
    pub response_body_size: u64,
    pub request_body_preview: Option<String>,
    pub response_body_preview: Option<String>,
    pub client_addr: String,
}

impl From<Transaction> for FfiTransaction {
    fn from(t: Transaction) -> Self {
        let headers = |v: Vec<relay_proxy::Header>| v.into_iter().map(|h| FfiHeader { name: h.name, value: h.value }).collect();
        FfiTransaction {
            url: t.url(),
            id: t.id,
            started_at_ms: t.started_at_ms,
            duration_ms: t.duration_ms,
            method: t.method,
            scheme: t.scheme,
            host: t.host,
            path: t.path,
            status: t.status,
            state: match t.state {
                TransactionState::Complete => FfiState::Complete,
                TransactionState::Failed => FfiState::Failed,
                TransactionState::Tunneled => FfiState::Tunneled,
                TransactionState::InProgress => FfiState::InProgress,
            },
            error: t.error,
            decrypted: t.decrypted,
            request_headers: headers(t.request_headers),
            response_headers: headers(t.response_headers),
            request_body_size: t.request_body_size,
            response_body_size: t.response_body_size,
            request_body_preview: t.request_body_preview,
            response_body_preview: t.response_body_preview,
            client_addr: t.client_addr,
        }
    }
}

/// Implemented by the app (Swift/Kotlin). Called on engine threads: hop to the UI thread yourself.
#[uniffi::export(callback_interface)]
pub trait TrafficListener: Send + Sync {
    fn on_transaction(&self, transaction: FfiTransaction);
}

struct ListenerSink(Box<dyn TrafficListener>);

impl EventSink for ListenerSink {
    fn on_transaction(&self, transaction: Transaction) {
        self.0.on_transaction(transaction.into());
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct VpnStats {
    pub tcp_connections: u64,
    pub udp_flows: u64,
    pub quic_blocked: u64,
}

#[derive(uniffi::Object)]
pub struct RelayEngine {
    runtime: tokio::runtime::Runtime,
    data_dir: PathBuf,
    handle: Mutex<Option<ProxyHandle>>,
    vpn: Mutex<Option<relay_netstack::VpnCapture>>,
}

#[uniffi::export]
impl RelayEngine {
    /// `data_dir` holds the CA certificate and key (created on first start).
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Result<Arc<Self>, RelayError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("relay-engine")
            .enable_all()
            .build()
            .map_err(|e| RelayError::Failed { reason: e.to_string() })?;
        Ok(Arc::new(Self { runtime, data_dir: data_dir.into(), handle: Mutex::new(None), vpn: Mutex::new(None) }))
    }

    /// Starts capturing from a VPN/TUN file descriptor (Android `VpnService`). Takes ownership of
    /// `tun_fd` (pass `ParcelFileDescriptor.detachFd()`). Call `start` first.
    pub fn start_vpn(&self, tun_fd: i32, mtu: u16, block_quic: bool) -> Result<(), RelayError> {
        let handler = match self.handle.lock().unwrap().as_ref() {
            Some(h) => h.transparent(),
            None => return Err(RelayError::Failed { reason: "start the engine before the VPN".into() }),
        };
        let _rt = self.runtime.enter();
        // SAFETY: the caller hands us exclusive ownership of an open TUN descriptor.
        let dev = unsafe { relay_netstack::TunFd::from_raw(tun_fd) }
            .map_err(|e| RelayError::Failed { reason: format!("TUN fd: {e}") })?;
        let capture = relay_netstack::start(dev, relay_netstack::NetstackOptions { mtu, block_quic, ..Default::default() }, handler)?;
        *self.vpn.lock().unwrap() = Some(capture);
        Ok(())
    }

    pub fn stop_vpn(&self) {
        if let Some(v) = self.vpn.lock().unwrap().take() {
            v.stop();
        }
    }

    pub fn vpn_stats(&self) -> Option<VpnStats> {
        use std::sync::atomic::Ordering::Relaxed;
        self.vpn.lock().unwrap().as_ref().map(|v| VpnStats {
            tcp_connections: v.stats.tcp_connections.load(Relaxed),
            udp_flows: v.stats.udp_flows.load(Relaxed),
            quic_blocked: v.stats.quic_blocked.load(Relaxed),
        })
    }

    /// Starts the proxy on `listen_ip:port` (port 0 = any free port). Returns the bound port.
    pub fn start(&self, listen_ip: String, port: u16, listener: Box<dyn TrafficListener>) -> Result<u16, RelayError> {
        let mut guard = self.handle.lock().unwrap();
        if let Some(h) = guard.as_ref() {
            return Ok(h.local_addr.port());
        }
        let addr = format!("{listen_ip}:{port}")
            .parse()
            .map_err(|e| RelayError::Failed { reason: format!("bad listen address: {e}") })?;
        let cfg = ProxyConfig::new(addr, &self.data_dir);
        let handle = self.runtime.block_on(relay_proxy::start(cfg, Arc::new(ListenerSink(listener))))?;
        let port = handle.local_addr.port();
        *guard = Some(handle);
        Ok(port)
    }

    pub fn stop(&self) {
        if let Some(mut h) = self.handle.lock().unwrap().take() {
            h.shutdown();
        }
    }

    pub fn is_running(&self) -> bool {
        self.handle.lock().unwrap().is_some()
    }

    /// PEM of the Relay root CA (creates it if needed).
    pub fn ca_certificate_pem(&self) -> Result<String, RelayError> {
        Ok(self.ca()?.ca_pem().to_owned())
    }

    /// Path of the CA certificate file, for `xcrun simctl keychain … add-root-cert`.
    pub fn ca_certificate_path(&self) -> Result<String, RelayError> {
        Ok(self.ca()?.ca_path().display().to_string())
    }
}

impl RelayEngine {
    fn ca(&self) -> Result<relay_proxy::ca::CertAuthority, RelayError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Ok(relay_proxy::ca::CertAuthority::load_or_create(&self.data_dir, provider)?)
    }
}

#[uniffi::export]
pub fn relay_core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
