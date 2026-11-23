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

fn phase_of(response: bool) -> relay_proxy::breakpoints::BreakPhase {
    if response {
        relay_proxy::breakpoints::BreakPhase::Response
    } else {
        relay_proxy::breakpoints::BreakPhase::Request
    }
}

fn rule_kind(a: &relay_model::rules::RuleAction) -> &'static str {
    use relay_model::rules::RuleAction::*;
    match a {
        Rewrite { .. } => "rewrite",
        MapLocal { .. } => "map local",
        MapRemote { .. } => "map remote",
        Breakpoint { .. } => "breakpoint",
        Block { .. } => "block",
        Throttle { .. } => "throttle",
    }
}

fn rule_summary(r: &relay_model::rules::Rule) -> String {
    let m = &r.matcher;
    let mut parts = vec![];
    if let Some(x) = &m.method {
        parts.push(x.clone());
    }
    if let Some(x) = &m.host {
        parts.push(x.clone());
    }
    if let Some(x) = &m.path {
        parts.push(x.clone());
    }
    if let Some(x) = &m.filter {
        parts.push(format!("[{x}]"));
    }
    if parts.is_empty() {
        "every request".into()
    } else {
        parts.join(" ")
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub kind: String,
    /// What it matches, e.g. `GET api.acme.com /v2/products`.
    pub summary: String,
}

/// A request or response held at a breakpoint.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPaused {
    pub tx_id: u64,
    /// True for a response, false for a request.
    pub response: bool,
    pub rule: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub headers: Vec<FfiHeader>,
    pub body: String,
    /// False for binary bodies, which cannot be edited as text.
    pub body_is_text: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiEdit {
    pub status: Option<u16>,
    pub headers: Vec<FfiHeader>,
    pub body: String,
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

impl RelayEngine {
    fn current_rules(&self) -> relay_model::rules::RuleSet {
        load_rules(&self.data_dir).unwrap_or(relay_model::rules::RuleSet {
            version: relay_model::rules::RULES_VERSION,
            rules: vec![],
        })
    }

    /// Saves `rules.yaml` (the given text, or the set re-serialised) and applies it live.
    fn save_rules(
        &self,
        set: relay_model::rules::RuleSet,
        yaml: Option<String>,
    ) -> Result<(), String> {
        let yaml = yaml.unwrap_or_else(|| set.to_yaml());
        std::fs::write(self.data_dir.join("rules.yaml"), yaml)
            .map_err(|e| format!("saving rules.yaml: {e}"))?;
        if let Some(h) = self.handle.lock().unwrap().as_ref() {
            h.set_rules(set, &self.data_dir);
        }
        Ok(())
    }
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
        match self.save_rules(set, Some(yaml)) {
            Ok(()) => vec![],
            Err(e) => vec![e],
        }
    }

    /// The current rules as YAML (a starter file when there are none).
    pub fn rules_yaml(&self) -> String {
        self.current_rules().to_yaml()
    }

    pub fn list_rules(&self) -> Vec<FfiRule> {
        self.current_rules()
            .rules
            .iter()
            .map(|r| FfiRule {
                id: r.id.clone(),
                name: r.name.clone(),
                enabled: r.enabled,
                kind: rule_kind(&r.action).into(),
                summary: rule_summary(r),
            })
            .collect()
    }

    /// Every rule whose match fits this request, switched on or off (so the UI can show which
    /// settings apply to an endpoint and which are disabled).
    pub fn rules_for(&self, request: FfiTransaction) -> Vec<FfiRule> {
        let t = Transaction::from(&request);
        self.current_rules()
            .rules
            .iter()
            .filter(|r| {
                r.matcher
                    .matches_request(&t.method, &t.scheme, &t.host, &t.path)
                    && r.matcher.filter.as_deref().is_none_or(|f| {
                        relay_model::filter::Filter::parse(f).is_ok_and(|f| f.matches(&t))
                    })
            })
            .map(|r| FfiRule {
                id: r.id.clone(),
                name: r.name.clone(),
                enabled: r.enabled,
                kind: rule_kind(&r.action).into(),
                summary: rule_summary(r),
            })
            .collect()
    }

    pub fn set_rule_enabled(&self, id: String, enabled: bool) -> Result<(), RelayError> {
        let mut set = self.current_rules();
        let rule = set
            .rules
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| RelayError::Failed {
                reason: format!("no rule `{id}`"),
            })?;
        rule.enabled = enabled;
        self.save_rules(set, None)
            .map_err(|reason| RelayError::Failed { reason })
    }

    pub fn remove_rule(&self, id: String) -> Result<(), RelayError> {
        let mut set = self.current_rules();
        set.rules.retain(|r| r.id != id);
        self.save_rules(set, None)
            .map_err(|reason| RelayError::Failed { reason })
    }

    /// Adds a rule aimed at one captured request. `kind`: `block`, `breakpoint` (request and
    /// response), `throttle-3g`, `offline`, or `map-local` (answers with the recorded response body,
    /// saved under `mocks/`). Returns the new rule's id.
    pub fn add_quick_rule(
        &self,
        kind: String,
        request: FfiTransaction,
    ) -> Result<String, RelayError> {
        let fail = |reason: String| RelayError::Failed { reason };
        use relay_model::rules::{Matcher, Phase, Rule, RuleAction, ThrottleProfile};
        let mut set = self.current_rules();
        let path = request.path.split('?').next().unwrap_or("/").to_owned();
        let host_slug: String = request
            .host
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let base = format!("{kind}-{host_slug}");
        let id = (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|c| !set.rules.iter().any(|r| &r.id == c))
            .unwrap();
        let action = match kind.as_str() {
            "block" => RuleAction::Block { status: 403 },
            "breakpoint" => RuleAction::Breakpoint { phase: Phase::Both },
            "throttle-3g" => RuleAction::Throttle {
                profile: ThrottleProfile::Preset("3g".into()),
            },
            "offline" => RuleAction::Throttle {
                profile: ThrottleProfile::Preset("offline".into()),
            },
            "map-local" => {
                let body = request
                    .response_body_preview
                    .clone()
                    .ok_or_else(|| fail("this response has no text body to save".into()))?;
                let json = request.response_headers.iter().any(|h| {
                    h.name.eq_ignore_ascii_case("content-type") && h.value.contains("json")
                });
                let rel = format!("mocks/{id}.{}", if json { "json" } else { "txt" });
                let full = self.data_dir.join(&rel);
                std::fs::create_dir_all(full.parent().unwrap()).map_err(|e| fail(e.to_string()))?;
                std::fs::write(&full, body).map_err(|e| fail(e.to_string()))?;
                RuleAction::MapLocal {
                    file: rel,
                    status: request.status.unwrap_or(200),
                    headers: Default::default(),
                }
            }
            other => return Err(fail(format!("unknown rule kind `{other}`"))),
        };
        set.rules.push(Rule {
            id: id.clone(),
            name: format!("{} {}{}", request.method, request.host, path),
            enabled: true,
            matcher: Matcher {
                method: Some(request.method.clone()),
                host: Some(request.host.clone()),
                path: Some(path),
                ..Default::default()
            },
            action,
        });
        self.save_rules(set, None).map_err(fail)?;
        Ok(id)
    }

    /// Requests and responses held at a breakpoint right now.
    pub fn paused(&self) -> Vec<FfiPaused> {
        self.handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.paused())
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let text = std::str::from_utf8(&p.body).is_ok();
                FfiPaused {
                    tx_id: p.tx_id,
                    response: p.phase == relay_proxy::breakpoints::BreakPhase::Response,
                    rule: p.rule,
                    method: p.method,
                    url: p.url,
                    status: p.status,
                    headers: p
                        .headers
                        .into_iter()
                        .map(|h| FfiHeader {
                            name: h.name,
                            value: h.value,
                        })
                        .collect(),
                    body: String::from_utf8_lossy(&p.body).into_owned(),
                    body_is_text: text,
                }
            })
            .collect()
    }

    /// Lets a held message go on. Pass `edit` to change it first; `None` sends it unchanged.
    pub fn resume_paused(&self, tx_id: u64, response: bool, edit: Option<FfiEdit>) -> bool {
        let edit = edit.map(|e| relay_proxy::breakpoints::Edit {
            status: e.status,
            headers: e
                .headers
                .into_iter()
                .map(|h| relay_proxy::Header {
                    name: h.name,
                    value: h.value,
                })
                .collect(),
            body: e.body.into_bytes(),
        });
        self.handle
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.resume(tx_id, phase_of(response), edit))
    }

    /// Cancels a held message; the app gets an error.
    pub fn abort_paused(&self, tx_id: u64, response: bool) -> bool {
        self.handle
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.abort(tx_id, phase_of(response)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FfiTransaction {
        FfiTransaction::from(Transaction {
            id: 1,
            started_at_ms: 0,
            duration_ms: 1,
            method: "GET".into(),
            scheme: "https".into(),
            host: "api.acme.com".into(),
            port: 443,
            path: "/v2/products?page=2".into(),
            status: Some(200),
            state: TransactionState::Complete,
            error: None,
            decrypted: true,
            http_version: "HTTP/2".into(),
            upstream_http_version: None,
            request_headers: vec![],
            response_headers: vec![relay_proxy::Header {
                name: "content-type".into(),
                value: "application/json".into(),
            }],
            request_body_size: 0,
            response_body_size: 11,
            request_body_preview: None,
            response_body_preview: Some(r#"{"ok":true}"#.into()),
            client_addr: "127.0.0.1:1".into(),
            websocket_messages: vec![],
            client_process: None,
            applied_rules: vec![],
        })
    }

    #[test]
    fn quick_rules_are_saved_listed_toggled_and_removed() {
        let dir = std::env::temp_dir().join(format!("relay-ffi-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = RelayEngine::new(dir.to_string_lossy().into()).unwrap();

        let block = engine.add_quick_rule("block".into(), sample()).unwrap();
        let again = engine.add_quick_rule("block".into(), sample()).unwrap();
        assert_ne!(block, again, "ids stay unique");
        let mock = engine.add_quick_rule("map-local".into(), sample()).unwrap();
        assert!(dir.join(format!("mocks/{mock}.json")).exists());
        assert!(engine.add_quick_rule("nonsense".into(), sample()).is_err());

        let rules = engine.list_rules();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].summary, "GET api.acme.com /v2/products");
        engine.set_rule_enabled(block.clone(), false).unwrap();
        assert!(!engine.list_rules()[0].enabled);
        // The saved file is valid YAML in the shared format.
        assert!(engine.set_rules(engine.rules_yaml()).is_empty());
        engine.remove_rule(block).unwrap();
        assert_eq!(engine.list_rules().len(), 2);
    }
}
