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
        DnsSpoof { .. } => "dns spoof",
    }
}

fn unique_rule_id(set: &relay_model::rules::RuleSet, kind: &str, host: &str) -> String {
    let slug: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let base = format!("{kind}-{slug}");
    (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|c| !set.rules.iter().any(|r| &r.id == c))
        .unwrap()
}

/// A rule aimed at one captured request (its method, host and path without the query).
fn rule_for(
    id: &str,
    request: &FfiTransaction,
    action: relay_model::rules::RuleAction,
) -> relay_model::rules::Rule {
    let path = request.path.split('?').next().unwrap_or("/").to_owned();
    relay_model::rules::Rule {
        id: id.to_owned(),
        name: format!("{} {}{}", request.method, request.host, path),
        enabled: true,
        matcher: relay_model::rules::Matcher {
            method: Some(request.method.clone()),
            host: Some(request.host.clone()),
            path: Some(path),
            ..Default::default()
        },
        action,
    }
}

fn rule_file(a: &relay_model::rules::RuleAction) -> (String, u16) {
    match a {
        relay_model::rules::RuleAction::MapLocal { file, status, .. } => (file.clone(), *status),
        _ => (String::new(), 0),
    }
}

/// Expands `~` and checks that the file can be read.
fn checked_file(path: &str) -> Result<String, RelayError> {
    let path = path.trim();
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|h| format!("{h}/{rest}"))
            .unwrap_or_else(|_| path.to_owned()),
        None => path.to_owned(),
    };
    match std::fs::metadata(&expanded) {
        Ok(m) if m.is_file() => Ok(expanded),
        Ok(_) => Err(RelayError::Failed {
            reason: format!("{expanded} is a folder, not a file"),
        }),
        Err(e) => Err(RelayError::Failed {
            reason: format!("cannot read {expanded}: {e}"),
        }),
    }
}

fn ffi_rule(r: &relay_model::rules::Rule) -> FfiRule {
    FfiRule {
        id: r.id.clone(),
        name: r.name.clone(),
        enabled: r.enabled,
        kind: rule_kind(&r.action).into(),
        summary: rule_summary(r),
        file: rule_file(&r.action).0,
        status: rule_file(&r.action).1,
        detail: rule_detail(&r.action),
    }
}

fn step_from(a: &relay_model::rules::RewriteAction) -> FfiRewriteStep {
    use relay_model::rules::RewriteAction::*;
    let blank = |kind: &str| FfiRewriteStep {
        kind: kind.into(),
        name: String::new(),
        value: String::new(),
        path: String::new(),
        find: String::new(),
        replace: String::new(),
        regex: false,
    };
    match a {
        SetJson { path, value } => FfiRewriteStep {
            path: path.clone(),
            value: value.to_string(),
            ..blank("set_json")
        },
        RemoveJson { path } => FfiRewriteStep {
            path: path.clone(),
            ..blank("remove_json")
        },
        SetHeader { name, value } => FfiRewriteStep {
            name: name.clone(),
            value: value.clone(),
            ..blank("set_header")
        },
        RemoveHeader { name } => FfiRewriteStep {
            name: name.clone(),
            ..blank("remove_header")
        },
        SetStatus(s) => FfiRewriteStep {
            value: s.to_string(),
            ..blank("set_status")
        },
        ReplaceBody {
            find,
            replace,
            regex,
        } => FfiRewriteStep {
            find: find.clone(),
            replace: replace.clone(),
            regex: *regex,
            ..blank("replace_body")
        },
        DelayMs(ms) => FfiRewriteStep {
            value: ms.to_string(),
            ..blank("delay")
        },
    }
}

fn step_to(s: &FfiRewriteStep) -> Result<relay_model::rules::RewriteAction, String> {
    use relay_model::rules::RewriteAction::*;
    let num = |what: &str| -> Result<u64, String> {
        s.value
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{what} must be a whole number, not `{}`", s.value))
    };
    Ok(match s.kind.as_str() {
        "set_json" => SetJson {
            path: s.path.trim().to_owned(),
            // Anything that is not valid JSON (a bare word) is taken as text.
            value: serde_json::from_str(s.value.trim())
                .unwrap_or_else(|_| serde_json::Value::String(s.value.clone())),
        },
        "remove_json" => RemoveJson {
            path: s.path.trim().to_owned(),
        },
        "set_header" => SetHeader {
            name: s.name.trim().to_owned(),
            value: s.value.clone(),
        },
        "remove_header" => RemoveHeader {
            name: s.name.trim().to_owned(),
        },
        "set_status" => SetStatus(num("status")? as u16),
        "replace_body" => ReplaceBody {
            find: s.find.clone(),
            replace: s.replace.clone(),
            regex: s.regex,
        },
        "delay" => DelayMs(num("delay")?),
        other => return Err(format!("unknown edit `{other}`")),
    })
}

fn rule_detail(a: &relay_model::rules::RuleAction) -> String {
    use relay_model::rules::{Phase, RuleAction};
    let RuleAction::Rewrite { phase, actions } = a else {
        return String::new();
    };
    let side = match phase {
        Phase::Request => "request",
        Phase::Response => "response",
        Phase::Both => "request and response",
    };
    let steps: Vec<String> = actions
        .iter()
        .map(|a| {
            let s = step_from(a);
            match s.kind.as_str() {
                "set_header" => format!("set header {} = {}", s.name, s.value),
                "remove_header" => format!("remove header {}", s.name),
                "set_status" => format!("status {}", s.value),
                "set_json" => format!("set {} = {}", s.path, s.value),
                "remove_json" => format!("remove {}", s.path),
                "replace_body" => format!("replace “{}” with “{}”", s.find, s.replace),
                _ => format!("delay {} ms", s.value),
            }
        })
        .collect();
    format!("{side}: {}", steps.join("; "))
}

fn rule_summary(r: &relay_model::rules::Rule) -> String {
    let m = &r.matcher;
    if let relay_model::rules::RuleAction::DnsSpoof { to } = &r.action {
        return format!("{} → {to}", m.host.as_deref().unwrap_or("?"));
    }
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
    /// The file a `map local` rule answers from (empty for other kinds).
    pub file: String,
    pub status: u16,
    /// For `rewrite` rules, what it changes, e.g. `response: set header x-env = staging; set $.price = 99`.
    pub detail: String,
}

/// One edit a rewrite rule makes. `kind` is `set_header`, `remove_header`, `set_status`,
/// `set_json`, `remove_json`, `replace_body` or `delay`; only the fields that kind uses matter.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRewriteStep {
    pub kind: String,
    /// Header name.
    pub name: String,
    /// Header value, a status code, a delay in ms, or (set_json) the JSON text of the new value.
    pub value: String,
    /// JSON path, e.g. `$.items[0].price`.
    pub path: String,
    pub find: String,
    pub replace: String,
    pub regex: bool,
}

/// A rewrite rule in editable form (what the Mac app's Rewrite editor works with).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRewriteRule {
    /// Empty for a new rule.
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Match only this method / host / path prefix (empty = any).
    pub method: String,
    pub host: String,
    pub path: String,
    /// `request`, `response` or `both`.
    pub phase: String,
    pub steps: Vec<FfiRewriteStep>,
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

/// A request to send through Relay (Compose / Repeat).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiComposeRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<FfiHeader>,
    pub body: String,
}

/// What came back for one composed request. `status` is 0 and `error` set when it never got a response.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiComposeResult {
    pub status: u16,
    pub error: Option<String>,
    pub headers: Vec<FfiHeader>,
    pub body_preview: Option<String>,
    pub body_size: u64,
    pub duration_ms: u64,
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
    /// Live switches that survive stopping and starting the proxy.
    decrypt: std::sync::atomic::AtomicBool,
    global_throttle: Mutex<Option<String>>,
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
            decrypt: std::sync::atomic::AtomicBool::new(true),
            global_throttle: Mutex::new(None),
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

    /// Sends `request` through Relay `count` times (1 to 200), one after another or all at once.
    /// The requests appear in the traffic list like any other, and rules and breakpoints apply.
    /// Blocks until all have finished: call it off the main thread.
    pub fn compose(
        &self,
        request: FfiComposeRequest,
        count: u32,
        parallel: bool,
    ) -> Result<Vec<FfiComposeResult>, RelayError> {
        let (addr, pem) = {
            let guard = self.handle.lock().unwrap();
            let h = guard.as_ref().ok_or_else(|| RelayError::Failed {
                reason: "start Relay first".into(),
            })?;
            (h.local_addr, h.ca_pem().to_owned())
        };
        // Always reach the proxy on loopback, whatever address it listens on.
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], addr.port()));
        let roots: Vec<_> = {
            use rustls::pki_types::{CertificateDer, pem::PemObject};
            CertificateDer::pem_slice_iter(pem.as_bytes())
                .filter_map(Result::ok)
                .collect()
        };
        let spec = relay_proxy::compose::ComposeRequest {
            method: request.method,
            url: request.url,
            headers: request
                .headers
                .into_iter()
                .map(|h| (h.name, h.value))
                .collect(),
            body: request.body.into_bytes(),
        };
        let count = count.clamp(1, 200) as usize;
        Ok(self.runtime.block_on(async move {
            let one = move |spec: relay_proxy::compose::ComposeRequest, roots: Vec<_>| async move {
                match relay_proxy::compose::send_via_proxy(addr, &roots, &spec).await {
                    Ok(r) => FfiComposeResult {
                        status: r.status,
                        error: None,
                        headers: r
                            .headers
                            .into_iter()
                            .map(|(name, value)| FfiHeader { name, value })
                            .collect(),
                        body_preview: r.body_preview,
                        body_size: r.body_size,
                        duration_ms: r.duration_ms,
                    },
                    Err(e) => FfiComposeResult {
                        status: 0,
                        error: Some(format!("{e:#}")),
                        headers: vec![],
                        body_preview: None,
                        body_size: 0,
                        duration_ms: 0,
                    },
                }
            };
            if !parallel {
                let mut out = Vec::with_capacity(count);
                for _ in 0..count {
                    out.push(one(spec.clone(), roots.clone()).await);
                }
                return out;
            }
            let mut set = tokio::task::JoinSet::new();
            for i in 0..count {
                let (s, r) = (spec.clone(), roots.clone());
                set.spawn(async move { (i, one(s, r).await) });
            }
            let mut done = Vec::with_capacity(count);
            while let Some(Ok(item)) = set.join_next().await {
                done.push(item);
            }
            done.sort_by_key(|(i, _)| *i);
            done.into_iter().map(|(_, r)| r).collect()
        }))
    }

    /// Turns HTTPS decryption on or off (off: HTTPS is tunnelled untouched). Remembered across restarts of the proxy.
    pub fn set_decrypt(&self, on: bool) {
        self.decrypt.store(on, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.lock().unwrap().as_ref() {
            h.set_decrypt(on);
        }
    }

    pub fn decrypt_enabled(&self) -> bool {
        self.decrypt.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Slows all traffic that no rule throttles; `None` turns it off. See `throttle_presets`.
    pub fn set_global_throttle(&self, preset: Option<String>) -> Result<(), RelayError> {
        if let Some(p) = &preset
            && !relay_model::rules::THROTTLE_PRESETS.contains(&p.as_str())
        {
            return Err(RelayError::Failed {
                reason: format!("unknown throttle preset `{p}`"),
            });
        }
        *self.global_throttle.lock().unwrap() = preset.clone();
        if let Some(h) = self.handle.lock().unwrap().as_ref() {
            h.set_global_throttle(preset)
                .map_err(|reason| RelayError::Failed { reason })?;
        }
        Ok(())
    }

    pub fn global_throttle(&self) -> Option<String> {
        self.global_throttle.lock().unwrap().clone()
    }

    pub fn throttle_presets(&self) -> Vec<String> {
        relay_model::rules::THROTTLE_PRESETS
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    /// Everything the filter matches (empty = all stored traffic), as a HAR 1.2 document, oldest first.
    pub fn export_har(&self, query: String) -> Result<String, RelayError> {
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
        let mut txs: Vec<_> = store
            .search(&filter, 1_000_000)?
            .into_iter()
            .map(|s| s.tx)
            .collect();
        txs.sort_by_key(|t| t.started_at_ms);
        Ok(relay_model::har::export(&txs))
    }

    /// Adds the exchanges of a HAR document to the stored history and returns them (ids are
    /// unique, so they can be shown next to live traffic).
    pub fn import_har(&self, har: String) -> Result<Vec<FfiTransaction>, RelayError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let txs = relay_model::har::import(&har, now_ms * 1000)
            .map_err(|reason| RelayError::Failed { reason })?;
        let store = self
            .store
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| RelayError::Failed {
                reason: "start the engine first".into(),
            })?;
        store.upsert(&format!("import-{now_ms}"), &txs)?;
        Ok(txs.into_iter().map(Into::into).collect())
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
        handle.set_decrypt(self.decrypt.load(std::sync::atomic::Ordering::Relaxed));
        let _ = handle.set_global_throttle(self.global_throttle.lock().unwrap().clone());
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
        self.current_rules().rules.iter().map(ffi_rule).collect()
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
            .map(ffi_rule)
            .collect()
    }

    /// Adds a `map local` rule for this request that answers from the file at `file`
    /// (absolute, `~/…`, or relative to the Relay data folder).
    pub fn add_map_local_rule(
        &self,
        request: FfiTransaction,
        file: String,
        status: u16,
    ) -> Result<String, RelayError> {
        let file = checked_file(&file)?;
        let mut set = self.current_rules();
        let id = unique_rule_id(&set, "map-local", &request.host);
        set.rules.push(rule_for(
            &id,
            &request,
            relay_model::rules::RuleAction::MapLocal {
                file,
                status: if status == 0 { 200 } else { status },
                headers: Default::default(),
            },
        ));
        self.save_rules(set, None)
            .map_err(|reason| RelayError::Failed { reason })?;
        Ok(id)
    }

    /// Adds a `dns_spoof` rule: connections Relay makes for `host` (and its subdomains) go to `to`
    /// (an IP address or another host name) instead. The app still sees `host`.
    pub fn add_dns_spoof_rule(&self, host: String, to: String) -> Result<String, RelayError> {
        let host = host.trim().to_owned();
        let mut set = self.current_rules();
        let id = unique_rule_id(&set, "dns", &host);
        set.rules.push(relay_model::rules::Rule {
            id: id.clone(),
            name: format!("{host} → {}", to.trim()),
            enabled: true,
            matcher: relay_model::rules::Matcher {
                host: Some(host),
                ..Default::default()
            },
            action: relay_model::rules::RuleAction::DnsSpoof {
                to: to.trim().to_owned(),
            },
        });
        let problems = set.validate();
        if let Some(p) = problems.first() {
            return Err(RelayError::Failed {
                reason: p.message.clone(),
            });
        }
        self.save_rules(set, None)
            .map_err(|reason| RelayError::Failed { reason })?;
        Ok(id)
    }

    /// A rewrite rule in editable form.
    pub fn get_rewrite_rule(&self, id: String) -> Result<FfiRewriteRule, RelayError> {
        use relay_model::rules::{Phase, RuleAction};
        let set = self.current_rules();
        let r = set
            .rules
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| RelayError::Failed {
                reason: format!("no rule `{id}`"),
            })?;
        let RuleAction::Rewrite { phase, actions } = &r.action else {
            return Err(RelayError::Failed {
                reason: format!("`{id}` is not a rewrite rule"),
            });
        };
        Ok(FfiRewriteRule {
            id: r.id.clone(),
            name: r.name.clone(),
            enabled: r.enabled,
            method: r.matcher.method.clone().unwrap_or_default(),
            host: r.matcher.host.clone().unwrap_or_default(),
            path: r.matcher.path.clone().unwrap_or_default(),
            phase: match phase {
                Phase::Request => "request",
                Phase::Response => "response",
                Phase::Both => "both",
            }
            .into(),
            steps: actions.iter().map(step_from).collect(),
        })
    }

    /// Creates a rewrite rule (empty `id`) or replaces the one with that id, and applies it live.
    /// Returns the rule's id. Problems (empty match, bad JSON path, …) come back as an error.
    pub fn save_rewrite_rule(&self, rule: FfiRewriteRule) -> Result<String, RelayError> {
        use relay_model::rules::{Matcher, Phase, Rule, RuleAction};
        let fail = |reason: String| RelayError::Failed { reason };
        let clean = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        };
        let phase = match rule.phase.as_str() {
            "request" => Phase::Request,
            "response" => Phase::Response,
            "both" => Phase::Both,
            other => {
                return Err(fail(format!(
                    "phase must be request, response or both, not `{other}`"
                )));
            }
        };
        let actions = rule
            .steps
            .iter()
            .map(step_to)
            .collect::<Result<Vec<_>, _>>()
            .map_err(fail)?;
        let mut set = self.current_rules();
        let id = if rule.id.is_empty() {
            let host = if rule.host.trim().is_empty() {
                "any"
            } else {
                rule.host.trim()
            };
            unique_rule_id(&set, "rewrite", host)
        } else {
            rule.id.clone()
        };
        let new = Rule {
            id: id.clone(),
            name: rule.name.trim().to_owned(),
            enabled: rule.enabled,
            matcher: Matcher {
                method: clean(&rule.method),
                host: clean(&rule.host),
                path: clean(&rule.path),
                ..Default::default()
            },
            action: RuleAction::Rewrite { phase, actions },
        };
        match set.rules.iter().position(|r| r.id == id) {
            Some(i) => {
                // Keep a matcher scheme/filter the editor does not show.
                let old = &set.rules[i].matcher;
                let mut m = new.matcher.clone();
                m.scheme = old.scheme.clone();
                m.filter = old.filter.clone();
                set.rules[i] = Rule { matcher: m, ..new };
            }
            None => set.rules.push(new),
        }
        if let Some(p) = set
            .validate()
            .into_iter()
            .find(|p| p.rule.as_deref() == Some(&id))
        {
            return Err(fail(p.message));
        }
        self.save_rules(set, None).map_err(fail)?;
        Ok(id)
    }

    /// Points an existing `map local` rule at another file and/or status.
    pub fn set_map_local_file(
        &self,
        id: String,
        file: String,
        status: u16,
    ) -> Result<(), RelayError> {
        let file = checked_file(&file)?;
        let mut set = self.current_rules();
        match set
            .rules
            .iter_mut()
            .find(|r| r.id == id)
            .map(|r| &mut r.action)
        {
            Some(relay_model::rules::RuleAction::MapLocal {
                file: f, status: s, ..
            }) => {
                *f = file;
                if status != 0 {
                    *s = status;
                }
            }
            _ => {
                return Err(RelayError::Failed {
                    reason: format!("`{id}` is not a map local rule"),
                });
            }
        }
        self.save_rules(set, None)
            .map_err(|reason| RelayError::Failed { reason })
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
        use relay_model::rules::{Phase, RuleAction, ThrottleProfile};
        let mut set = self.current_rules();
        let id = unique_rule_id(&set, &kind, &request.host);
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
        set.rules.push(rule_for(&id, &request, action));
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
        // Map local to a file the user picked.
        let file = dir.join("custom.json");
        std::fs::write(&file, "{}").unwrap();
        let id = engine
            .add_map_local_rule(sample(), file.to_string_lossy().into(), 202)
            .unwrap();
        let r = engine
            .list_rules()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap();
        assert_eq!((r.kind.as_str(), r.status), ("map local", 202));
        assert_eq!(r.file, file.to_string_lossy());
        assert!(
            engine
                .add_map_local_rule(sample(), "/no/such/file.json".into(), 200)
                .is_err()
        );
        assert!(
            engine
                .set_map_local_file(id.clone(), "/no/such".into(), 0)
                .is_err()
        );
        engine.remove_rule(id).unwrap();
        engine.remove_rule(block).unwrap();
        assert_eq!(engine.list_rules().len(), 2);
    }
    struct Quiet;
    impl TrafficListener for Quiet {
        fn on_transaction(&self, _transaction: FfiTransaction) {}
    }

    #[test]
    fn har_import_lands_in_history_and_exports_again() {
        let dir = std::env::temp_dir().join(format!("relay-ffi-har-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = RelayEngine::new(dir.to_string_lossy().into()).unwrap();
        engine
            .start("127.0.0.1".into(), 0, Box::new(Quiet))
            .unwrap();

        let har = r#"{"log":{"entries":[{"startedDateTime":"2026-01-02T03:04:05Z","time":30,
            "request":{"method":"GET","url":"https://api.acme.com/v2/items?page=2","httpVersion":"h2","headers":[],"bodySize":0},
            "response":{"status":404,"headers":[],"content":{"size":2,"text":"{}"},"bodySize":2}}]}}"#;
        let shown = engine.import_har(har.into()).unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].status, Some(404));

        let out = engine
            .export_har("host:api.acme.com status:4xx".into())
            .unwrap();
        assert!(
            out.contains("https://api.acme.com/v2/items?page=2"),
            "{out}"
        );
        assert!(engine.import_har("not har".into()).is_err());
        engine.stop();
    }
    #[test]
    fn compose_needs_a_running_engine_and_rejects_bad_urls() {
        let dir = std::env::temp_dir().join(format!("relay-ffi-compose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = RelayEngine::new(dir.to_string_lossy().into()).unwrap();
        let req = FfiComposeRequest {
            method: "GET".into(),
            url: "ftp://nope".into(),
            headers: vec![],
            body: String::new(),
        };
        assert!(
            engine.compose(req.clone(), 1, false).is_err(),
            "not started"
        );
        engine
            .start("127.0.0.1".into(), 0, Box::new(Quiet))
            .unwrap();
        let out = engine.compose(req, 3, true).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|r| r.status == 0 && r.error.is_some()));
        engine.stop();
    }
    #[test]
    fn dns_spoof_rules_are_validated_and_listed() {
        let dir = std::env::temp_dir().join(format!("relay-ffi-dns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = RelayEngine::new(dir.to_string_lossy().into()).unwrap();
        assert!(
            engine
                .add_dns_spoof_rule("api.acme.com".into(), "http://x".into())
                .is_err()
        );
        assert!(
            engine
                .add_dns_spoof_rule("".into(), "127.0.0.1".into())
                .is_err()
        );
        let id = engine
            .add_dns_spoof_rule("api.acme.com".into(), " 127.0.0.1 ".into())
            .unwrap();
        let r = engine
            .list_rules()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap();
        assert_eq!(r.kind, "dns spoof");
        assert_eq!(r.summary, "api.acme.com → 127.0.0.1");
    }
    #[test]
    fn rewrite_rules_are_created_edited_and_validated() {
        let dir = std::env::temp_dir().join(format!("relay-ffi-rewrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = RelayEngine::new(dir.to_string_lossy().into()).unwrap();
        let step = |kind: &str| FfiRewriteStep {
            kind: kind.into(),
            name: String::new(),
            value: String::new(),
            path: String::new(),
            find: String::new(),
            replace: String::new(),
            regex: false,
        };
        let rule = FfiRewriteRule {
            id: String::new(),
            name: "Cheaper shoes".into(),
            enabled: true,
            method: "GET".into(),
            host: "api.acme.com".into(),
            path: "/v2/products".into(),
            phase: "response".into(),
            steps: vec![
                FfiRewriteStep {
                    name: "x-env".into(),
                    value: "staging".into(),
                    ..step("set_header")
                },
                FfiRewriteStep {
                    path: "$.items[0].price".into(),
                    value: "99.5".into(),
                    ..step("set_json")
                },
                FfiRewriteStep {
                    path: "$.items[0].name".into(),
                    value: "Trail Runner".into(),
                    ..step("set_json")
                },
                FfiRewriteStep {
                    value: "503".into(),
                    ..step("set_status")
                },
            ],
        };
        let id = engine.save_rewrite_rule(rule.clone()).unwrap();
        let listed = engine
            .list_rules()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap();
        assert_eq!(listed.kind, "rewrite");
        assert!(
            listed
                .detail
                .starts_with("response: set header x-env = staging; set $.items[0].price = 99.5"),
            "{}",
            listed.detail
        );
        // Bare words become text, numbers stay numbers.
        let back = engine.get_rewrite_rule(id.clone()).unwrap();
        assert_eq!(back.steps[1].value, "99.5");
        assert_eq!(back.steps[2].value, "\"Trail Runner\"");

        // Edit in place: same id, new value, turned off.
        let mut edited = back.clone();
        edited.enabled = false;
        edited.steps[0].value = "prod".into();
        assert_eq!(engine.save_rewrite_rule(edited).unwrap(), id);
        let now = engine.get_rewrite_rule(id.clone()).unwrap();
        assert!(!now.enabled);
        assert_eq!(now.steps[0].value, "prod");
        assert_eq!(engine.list_rules().len(), 1);

        // Problems are reported, nothing is saved.
        let mut bad = rule.clone();
        bad.host = String::new();
        bad.path = String::new();
        bad.method = String::new();
        assert!(engine.save_rewrite_rule(bad).is_err());
        let mut bad = rule.clone();
        bad.steps = vec![FfiRewriteStep {
            value: "abc".into(),
            ..step("set_status")
        }];
        assert!(engine.save_rewrite_rule(bad).is_err());
        assert_eq!(engine.list_rules().len(), 1);
        assert!(engine.get_rewrite_rule("nope".into()).is_err());
    }

    #[test]
    fn island_switches_are_remembered_and_validated() {
        let dir = std::env::temp_dir().join(format!("relay-ffi-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = RelayEngine::new(dir.to_string_lossy().into()).unwrap();
        assert!(engine.decrypt_enabled());
        engine.set_decrypt(false);
        engine.set_global_throttle(Some("3g".into())).unwrap();
        assert!(engine.set_global_throttle(Some("warp".into())).is_err());
        assert!(engine.throttle_presets().contains(&"offline".to_string()));
        // Set before the proxy starts, applied when it does.
        engine
            .start("127.0.0.1".into(), 0, Box::new(Quiet))
            .unwrap();
        assert!(!engine.decrypt_enabled());
        assert_eq!(engine.global_throttle().as_deref(), Some("3g"));
        engine.set_global_throttle(None).unwrap();
        engine.stop();
    }
}
