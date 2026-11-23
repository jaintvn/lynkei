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
        RelayError::Failed {
            reason: format!("{e:#}"),
        }
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
    pub port: u16,
    pub path: String,
    pub status: Option<u16>,
    pub state: FfiState,
    pub error: Option<String>,
    pub decrypted: bool,
    pub http_version: String,
    pub upstream_http_version: Option<String>,
    pub request_headers: Vec<FfiHeader>,
    pub response_headers: Vec<FfiHeader>,
    pub request_body_size: u64,
    pub response_body_size: u64,
    pub request_body_preview: Option<String>,
    pub response_body_preview: Option<String>,
    pub client_addr: String,
    pub websocket_messages: Vec<FfiWsMessage>,
    /// The program on this Mac that made the request, when known.
    pub client_process: Option<String>,
    /// Ids of the rules that changed or answered this exchange.
    pub applied_rules: Vec<String>,
}

impl From<Transaction> for FfiTransaction {
    fn from(t: Transaction) -> Self {
        let headers = |v: Vec<relay_proxy::Header>| {
            v.into_iter()
                .map(|h| FfiHeader {
                    name: h.name,
                    value: h.value,
                })
                .collect()
        };
        FfiTransaction {
            url: t.url(),
            id: t.id,
            started_at_ms: t.started_at_ms,
            duration_ms: t.duration_ms,
            method: t.method,
            scheme: t.scheme,
            host: t.host,
            port: t.port,
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
            http_version: t.http_version,
            upstream_http_version: t.upstream_http_version,
            request_headers: headers(t.request_headers),
            response_headers: headers(t.response_headers),
            request_body_size: t.request_body_size,
            response_body_size: t.response_body_size,
            request_body_preview: t.request_body_preview,
            response_body_preview: t.response_body_preview,
            client_addr: t.client_addr,
            websocket_messages: t
                .websocket_messages
                .into_iter()
                .map(|m| FfiWsMessage {
                    sent: m.direction == relay_proxy::WsDirection::Send,
                    kind: format!("{:?}", m.kind).to_lowercase(),
                    size: m.size,
                    preview: m.preview,
                    at_ms: m.at_ms,
                })
                .collect(),
            client_process: t.client_process,
            applied_rules: t.applied_rules,
        }
    }
}

/// Back to the model type (WebSocket frames are dropped; filters do not look at them).
impl From<&FfiTransaction> for Transaction {
    fn from(t: &FfiTransaction) -> Self {
        let headers = |v: &[FfiHeader]| {
            v.iter()
                .map(|h| relay_proxy::Header {
                    name: h.name.clone(),
                    value: h.value.clone(),
                })
                .collect()
        };
        Transaction {
            id: t.id,
            started_at_ms: t.started_at_ms,
            duration_ms: t.duration_ms,
            method: t.method.clone(),
            scheme: t.scheme.clone(),
            host: t.host.clone(),
            port: t.port,
            path: t.path.clone(),
            status: t.status,
            state: match t.state {
                FfiState::Complete => TransactionState::Complete,
                FfiState::Failed => TransactionState::Failed,
                FfiState::Tunneled => TransactionState::Tunneled,
                FfiState::InProgress => TransactionState::InProgress,
            },
            error: t.error.clone(),
            decrypted: t.decrypted,
            http_version: t.http_version.clone(),
            upstream_http_version: t.upstream_http_version.clone(),
            request_headers: headers(&t.request_headers),
            response_headers: headers(&t.response_headers),
            request_body_size: t.request_body_size,
            response_body_size: t.response_body_size,
            request_body_preview: t.request_body_preview.clone(),
            response_body_preview: t.response_body_preview.clone(),
            client_addr: t.client_addr.clone(),
            websocket_messages: vec![],
            client_process: t.client_process.clone(),
            applied_rules: t.applied_rules.clone(),
        }
    }
}

/// A parsed search query (`app:AcmeShop host:httpbin.org status:5xx`), for filtering a list in the UI.
#[derive(uniffi::Object)]
pub struct TrafficFilter {
    inner: relay_model::filter::Filter,
}

#[uniffi::export]
impl TrafficFilter {
    /// Fails with a readable message when the query is not valid.
    #[uniffi::constructor]
    pub fn parse(query: String) -> Result<Arc<Self>, RelayError> {
        let inner = relay_model::filter::Filter::parse(&query).map_err(|e| RelayError::Failed {
            reason: e.to_string(),
        })?;
        Ok(Arc::new(Self { inner }))
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn matches(&self, transaction: FfiTransaction) -> bool {
        self.inner.matches(&Transaction::from(&transaction))
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWsMessage {
    /// true = app → server.
    pub sent: bool,
    /// "text", "binary", "close", "ping", "pong", "continuation" or "other".
    pub kind: String,
    pub size: u64,
    pub preview: Option<String>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDevice {
    pub id: String,
    /// "ios_simulator", "android_emulator" or "android_usb".
    pub kind: String,
    pub name: String,
    pub os: String,
    pub connected: bool,
}

/// One line of the connect checklist.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStep {
    /// "pending", "done", "needs_action" or "failed".
    pub state: String,
    pub detail: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiConnectResult {
    pub device: FfiDevice,
    pub proxy: FfiStep,
    pub certificate: FfiStep,
    pub ready: bool,
    pub connected_at_ms: u64,
}

fn step(s: relay_devices::StepState) -> FfiStep {
    use relay_devices::StepState::*;
    match s {
        Pending => FfiStep {
            state: "pending".into(),
            detail: String::new(),
        },
        Done(d) => FfiStep {
            state: "done".into(),
            detail: d,
        },
        NeedsAction(d) => FfiStep {
            state: "needs_action".into(),
            detail: d,
        },
        Failed(d) => FfiStep {
            state: "failed".into(),
            detail: d,
        },
    }
}

fn device(d: relay_devices::Device) -> FfiDevice {
    let kind = match d.kind {
        relay_devices::Kind::IosSimulator => "ios_simulator",
        relay_devices::Kind::AndroidEmulator => "android_emulator",
        relay_devices::Kind::AndroidUsb => "android_usb",
    };
    FfiDevice {
        id: d.id,
        kind: kind.into(),
        name: d.name,
        os: d.os,
        connected: d.connected,
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

fn load_rules(dir: &std::path::Path) -> Option<relay_model::rules::RuleSet> {
    let text = std::fs::read_to_string(dir.join("rules.yaml")).ok()?;
    let set = relay_model::rules::RuleSet::from_yaml(&text).ok()?;
    set.validate().is_empty().then_some(set)
}

#[derive(uniffi::Object)]
pub struct RelayEngine {
    runtime: tokio::runtime::Runtime,
    data_dir: PathBuf,
    handle: Mutex<Option<ProxyHandle>>,
    vpn: Mutex<Option<relay_netstack::VpnCapture>>,
    store: Mutex<Option<Arc<relay_store::Store>>>,
    devices: Arc<relay_devices::DeviceManager>,
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
            .map_err(|e| RelayError::Failed {
                reason: e.to_string(),
            })?;
        // On macOS, simulators use the Mac's system proxy; creating the manager also puts back any
        // setting a crashed earlier run left behind.
        let data_dir: PathBuf = data_dir.into();
        let switch: Option<Arc<dyn relay_devices::SystemProxySwitch>> = if cfg!(target_os = "macos")
        {
            relay_control::sysproxy::SystemProxy::new(
                Arc::new(relay_control::sysproxy::Networksetup),
                &data_dir,
            )
            .ok()
            .map(|s| Arc::new(s) as Arc<dyn relay_devices::SystemProxySwitch>)
        } else {
            None
        };
        let devices = Arc::new(relay_devices::DeviceManager::system(
            switch,
            &data_dir.join(relay_proxy::ca::CA_CERT_FILE),
        ));
        Ok(Arc::new(Self {
            devices,
            runtime,
            data_dir,
            handle: Mutex::new(None),
            vpn: Mutex::new(None),
            store: Mutex::new(None),
        }))
    }

    /// Starts capturing from a VPN/TUN file descriptor (Android `VpnService`). Takes ownership of
    /// `tun_fd` (pass `ParcelFileDescriptor.detachFd()`). Call `start` first.
    pub fn start_vpn(&self, tun_fd: i32, mtu: u16, block_quic: bool) -> Result<(), RelayError> {
        let handler = match self.handle.lock().unwrap().as_ref() {
            Some(h) => h.transparent(),
            None => {
                return Err(RelayError::Failed {
                    reason: "start the engine before the VPN".into(),
                });
            }
        };
        let _rt = self.runtime.enter();
        // SAFETY: the caller hands us exclusive ownership of an open TUN descriptor.
        let dev =
            unsafe { relay_netstack::TunFd::from_raw(tun_fd) }.map_err(|e| RelayError::Failed {
                reason: format!("TUN fd: {e}"),
            })?;
        let capture = relay_netstack::start(
            dev,
            relay_netstack::NetstackOptions {
                mtu,
                block_quic,
                ..Default::default()
            },
            handler,
        )?;
        *self.vpn.lock().unwrap() = Some(capture);
        Ok(())
    }

    pub fn stop_vpn(&self) {
        if let Some(v) = self.vpn.lock().unwrap().take() {
            v.stop();
        }
    }

    /// Searches the stored history with the filter language (`status:5xx host:api.acme.com …`), newest first.
    pub fn search(&self, query: String, limit: u32) -> Result<Vec<FfiTransaction>, RelayError> {
        let filter =
            relay_model::filter::Filter::parse(&query).map_err(|e| RelayError::Failed {
                reason: e.to_string(),
            })?;
        let store = self
            .store
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| RelayError::Failed {
                reason: "start the engine first".into(),
            })?;
        Ok(store
            .search(&filter, limit as usize)?
            .into_iter()
            .map(|s| s.tx.into())
            .collect())
    }

    /// Checks a filter without running it; returns the error message, or `None` when valid.
    pub fn validate_filter(&self, query: String) -> Option<String> {
        relay_model::filter::Filter::parse(&query)
            .err()
            .map(|e| e.to_string())
    }

    pub fn stored_count(&self) -> u64 {
        self.store
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.count().ok())
            .unwrap_or(0)
    }

    /// Booted iOS simulators and attached Android devices (runs simctl/adb; takes a moment).
    pub fn list_devices(&self) -> Vec<FfiDevice> {
        self.devices.list().into_iter().map(device).collect()
    }

    /// Connects a device to the running proxy and returns the checklist.
    pub fn connect_device(&self, id: String) -> Result<FfiConnectResult, RelayError> {
        let port = self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.local_addr.port())
            .ok_or_else(|| RelayError::Failed {
                reason: "start the engine first".into(),
            })?;
        let r = self.devices.connect(&id, port)?;
        let ready = r.checklist.ready();
        Ok(FfiConnectResult {
            device: device(r.device),
            proxy: step(r.checklist.proxy),
            certificate: step(r.checklist.certificate),
            ready,
            connected_at_ms: r.connected_at_ms,
        })
    }

    /// The checklist of a device that is already connected (`None` if it is not).
    pub fn device_status(&self, id: String) -> Option<FfiConnectResult> {
        let r = self.devices.result(&id)?;
        let ready = r.checklist.ready();
        Some(FfiConnectResult {
            device: device(r.device),
            proxy: step(r.checklist.proxy),
            certificate: step(r.checklist.certificate),
            ready,
            connected_at_ms: r.connected_at_ms,
        })
    }

    pub fn disconnect_device(&self, id: String) -> Result<(), RelayError> {
        Ok(self.devices.disconnect(&id)?)
    }

    /// Undoes every device connection (call when the app quits).
    pub fn disconnect_all_devices(&self) {
        self.devices.disconnect_all();
    }

    /// `http://<mac>.local:<port>/setup` — what the iPhone QR code encodes.
    pub fn wifi_setup_url(&self) -> String {
        let port = self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.local_addr.port())
            .unwrap_or(8888);
        relay_devices::wifi_setup_url(&relay_devices::SystemRunner, port)
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
    pub fn start(
        &self,
        listen_ip: String,
        port: u16,
        listener: Box<dyn TrafficListener>,
    ) -> Result<u16, RelayError> {
        let mut guard = self.handle.lock().unwrap();
        if let Some(h) = guard.as_ref() {
            return Ok(h.local_addr.port());
        }
        let addr = format!("{listen_ip}:{port}")
            .parse()
            .map_err(|e| RelayError::Failed {
                reason: format!("bad listen address: {e}"),
            })?;
        let cfg = ProxyConfig::new(addr, &self.data_dir);
        // Every event is persisted (searchable history) and forwarded to the app.
        let store = relay_store::Store::open(
            &self.data_dir.join("relay.db"),
            relay_store::StoreConfig::default(),
        )?;
        let session = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let sink = relay_store::StoreSink::new(
            store.clone(),
            session,
            Some(Arc::new(ListenerSink(listener))),
        );
        *self.store.lock().unwrap() = Some(store);
        let handle = self.runtime.block_on(relay_proxy::start(cfg, sink))?;
        if let Some(set) = load_rules(&self.data_dir) {
            handle.set_rules(set, &self.data_dir);
        }
        let port = handle.local_addr.port();
        *guard = Some(handle);
        Ok(port)
    }

    /// Validates and applies rules (the YAML format in `rules.yaml`) and saves them. Returns the
    /// problems found; nothing is applied unless the list is empty.
    pub fn set_rules(&self, yaml: String) -> Vec<String> {
        let set = match relay_model::rules::RuleSet::from_yaml(&yaml) {
            Ok(s) => s,
            Err(e) => return vec![e],
        };
        let problems: Vec<String> = set.validate().iter().map(|p| p.to_string()).collect();
        if !problems.is_empty() {
            return problems;
        }
        if let Err(e) = std::fs::write(self.data_dir.join("rules.yaml"), yaml) {
            return vec![format!("saving rules.yaml: {e}")];
        }
        if let Some(h) = self.handle.lock().unwrap().as_ref() {
            h.set_rules(set, &self.data_dir);
        }
        vec![]
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
        Ok(relay_proxy::ca::CertAuthority::load_or_create(
            &self.data_dir,
            provider,
        )?)
    }
}

#[uniffi::export]
pub fn relay_core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
