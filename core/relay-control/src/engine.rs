//! The engine relayd hosts: proxy + store + rules + live events + system proxy, behind one API.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use relay_model::Transaction;
use relay_model::filter::Filter;
use relay_model::rules::{RULES_VERSION, RuleSet};
use relay_proxy::breakpoints::{BreakPhase, Edit, Paused};
use relay_proxy::{EventSink, ProxyConfig, ProxyHandle};
use relay_store::{Store, StoreConfig, StoreSink};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};

use crate::RpcError;
use crate::sysproxy::{SystemProxy, SystemProxyBackend};

pub const RULES_FILE: &str = "rules.yaml";

#[derive(Clone)]
pub struct EngineOptions {
    pub data_dir: PathBuf,
    /// Extra upstream roots (tests).
    pub extra_upstream_roots: Vec<relay_proxy::CertificateDer<'static>>,
    pub store: StoreConfig,
    /// How the macOS system proxy is changed (real `networksetup`, or a fake in tests).
    pub system_proxy: Option<Arc<dyn SystemProxyBackend>>,
    /// Runs simctl/adb/openssl (a fake in tests). Default: real commands.
    pub devices_runner: Option<Arc<dyn relay_devices::Runner>>,
    /// Path to `adb`; found automatically when `None`.
    pub adb: Option<String>,
}

impl EngineOptions {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            extra_upstream_roots: vec![],
            store: StoreConfig::default(),
            system_proxy: None,
            devices_runner: None,
            adb: None,
        }
    }
}

pub struct Engine {
    opts: EngineOptions,
    store: Arc<Store>,
    sink: Arc<StoreSink>,
    events: broadcast::Sender<Transaction>,
    proxy: Mutex<Option<ProxyHandle>>,
    rules: std::sync::Mutex<RuleSet>,
    /// Live switches, kept across proxy restarts: HTTPS decryption and a network preset for everything.
    switches: std::sync::Mutex<Switches>,
    system_proxy: Option<Arc<SystemProxy>>,
    devices: Arc<relay_devices::DeviceManager>,
    devices_runner: Arc<dyn relay_devices::Runner>,
}

#[derive(Clone)]
struct Switches {
    decrypt: bool,
    throttle: Option<String>,
}

struct Broadcast(broadcast::Sender<Transaction>);

impl EventSink for Broadcast {
    fn on_transaction(&self, t: Transaction) {
        let _ = self.0.send(t);
    }
}

impl Engine {
    pub fn new(opts: EngineOptions) -> Result<Arc<Engine>> {
        std::fs::create_dir_all(&opts.data_dir)
            .with_context(|| format!("creating {}", opts.data_dir.display()))?;
        let store = Store::open(&opts.data_dir.join("relay.db"), opts.store.clone())?;
        let (events, _) = broadcast::channel(4096);
        let session = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
            .to_string();
        let sink = StoreSink::new(
            store.clone(),
            session,
            Some(Arc::new(Broadcast(events.clone()))),
        );
        let rules = load_rules(&opts.data_dir.join(RULES_FILE))?;
        let system_proxy = match &opts.system_proxy {
            Some(backend) => Some(Arc::new(SystemProxy::new(backend.clone(), &opts.data_dir)?)),
            None => None,
        };
        let devices_runner: Arc<dyn relay_devices::Runner> = opts
            .devices_runner
            .clone()
            .unwrap_or_else(|| Arc::new(relay_devices::SystemRunner));
        let adb = opts
            .adb
            .clone()
            .or_else(|| relay_devices::find_adb().map(|p| p.to_string_lossy().into_owned()));
        let switch: Option<Arc<dyn relay_devices::SystemProxySwitch>> = system_proxy
            .clone()
            .map(|s| s as Arc<dyn relay_devices::SystemProxySwitch>);
        let devices = Arc::new(
            relay_devices::DeviceManager::new(
                devices_runner.clone(),
                adb,
                switch,
                &opts.data_dir.join(relay_proxy::ca::CA_CERT_FILE),
            )
            .with_state_file(&opts.data_dir.join("android-devices.json")),
        );
        Ok(Arc::new(Engine {
            opts,
            store,
            sink,
            events,
            proxy: Mutex::new(None),
            rules: std::sync::Mutex::new(rules),
            switches: std::sync::Mutex::new(Switches {
                decrypt: true,
                throttle: None,
            }),
            system_proxy,
            devices,
            devices_runner,
        }))
    }

    pub fn data_dir(&self) -> &Path {
        &self.opts.data_dir
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Transaction> {
        self.events.subscribe()
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Waits until every event so far is searchable.
    pub fn flush(&self) {
        self.sink.flush();
    }

    pub async fn start_proxy(&self, listen: SocketAddr) -> Result<u16> {
        let mut guard = self.proxy.lock().await;
        if let Some(h) = guard.as_ref() {
            return Ok(h.local_addr.port());
        }
        let mut cfg = ProxyConfig::new(listen, &self.opts.data_dir);
        cfg.extra_upstream_roots = self.opts.extra_upstream_roots.clone();
        let handle = relay_proxy::start(cfg, self.sink.clone()).await?;
        handle.set_rules(self.rules.lock().unwrap().clone(), &self.opts.data_dir);
        let sw = self.switches.lock().unwrap().clone();
        handle.set_decrypt(sw.decrypt);
        let _ = handle.set_global_throttle(sw.throttle);
        let port = handle.local_addr.port();
        *guard = Some(handle);
        Ok(port)
    }

    pub async fn proxy_port(&self) -> Option<u16> {
        self.proxy
            .lock()
            .await
            .as_ref()
            .map(|h| h.local_addr.port())
    }

    pub async fn stop_proxy(&self) {
        if let Some(mut h) = self.proxy.lock().await.take() {
            h.shutdown();
        }
    }

    pub fn system_proxy(&self) -> Option<&SystemProxy> {
        self.system_proxy.as_deref()
    }

    /// Restores system settings Relay changed. Call on shutdown.
    pub async fn shutdown(&self) {
        let devices = self.devices.clone();
        let _ = tokio::task::spawn_blocking(move || devices.disconnect_all()).await;
        if let Some(sp) = &self.system_proxy {
            let _ = sp.disable();
        }
        self.stop_proxy().await;
        self.flush();
    }

    /// Dispatches one control call.
    pub async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let engine_err = |e: anyhow::Error| RpcError::new(RpcError::ENGINE, format!("{e:#}"));
        let bad = |m: &str| RpcError::new(RpcError::INVALID_PARAMS, m.to_string());
        match method {
            "hello" => {
                let port = self.proxy_port().await;
                Ok(json!({
                    "api": crate::API_VERSION,
                    "engine_version": env!("CARGO_PKG_VERSION"),
                    "proxy": { "running": port.is_some(), "port": port },
                    "store": { "transactions": self.store.count().map_err(engine_err)?, "dropped": self.sink.dropped() },
                    "ca_path": self.opts.data_dir.join(relay_proxy::ca::CA_CERT_FILE),
                    "system_proxy": self.system_proxy.as_ref().map(|s| s.status_json()),
                    "rules": self.rules.lock().unwrap().rules.len(),
                }))
            }
            "proxy.start" => {
                let port = params.get("port").and_then(Value::as_u64).unwrap_or(8888) as u16;
                let ip = params
                    .get("listen")
                    .and_then(Value::as_str)
                    .unwrap_or("127.0.0.1");
                let addr: SocketAddr = format!("{ip}:{port}")
                    .parse()
                    .map_err(|_| bad("bad listen address"))?;
                let port = self.start_proxy(addr).await.map_err(engine_err)?;
                Ok(json!({ "port": port }))
            }
            "proxy.stop" => {
                self.stop_proxy().await;
                Ok(json!({}))
            }
            "traffic.search" => {
                let query = params.get("query").and_then(Value::as_str).unwrap_or("");
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100)
                    .min(10_000) as usize;
                let filter = Filter::parse(query).map_err(|e| bad(&e.to_string()))?;
                self.flush();
                let rows = self.store.search(&filter, limit).map_err(engine_err)?;
                Ok(Value::Array(
                    rows.into_iter()
                        .map(|r| json!({"key": r.key, "session": r.session, "transaction": r.tx}))
                        .collect(),
                ))
            }
            "traffic.get" => {
                let key = params
                    .get("key")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| bad("`key` is required"))?;
                self.flush();
                match self.store.get(key).map_err(engine_err)? {
                    Some(r) => Ok(json!({"key": r.key, "session": r.session, "transaction": r.tx})),
                    None => Err(RpcError::new(
                        RpcError::ENGINE,
                        format!("no transaction {key}"),
                    )),
                }
            }
            "compose.send" => {
                // Sends a request through the running proxy, so it shows in the list and rules apply.
                let (addr, pem) = {
                    let guard = self.proxy.lock().await;
                    let h = guard
                        .as_ref()
                        .ok_or_else(|| RpcError::new(RpcError::ENGINE, "start the proxy first"))?;
                    (h.local_addr, h.ca_pem().to_owned())
                };
                let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
                let text = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or("");
                let url = text("url").to_string();
                if url.is_empty() {
                    return Err(bad("`url` is required"));
                }
                let method = match text("method") {
                    "" => "GET".to_string(),
                    m => m.to_uppercase(),
                };
                let mut headers = Vec::new();
                match params.get("headers") {
                    Some(Value::Array(a)) => {
                        for h in a {
                            if let (Some(n), Some(v)) = (
                                h.get("name").and_then(Value::as_str),
                                h.get("value").and_then(Value::as_str),
                            ) {
                                headers.push((n.to_string(), v.to_string()));
                            }
                        }
                    }
                    Some(Value::Object(m)) => {
                        for (n, v) in m {
                            if let Some(v) = v.as_str() {
                                headers.push((n.clone(), v.to_string()));
                            }
                        }
                    }
                    _ => {}
                }
                let spec = relay_proxy::compose::ComposeRequest {
                    method,
                    url,
                    headers,
                    body: text("body").as_bytes().to_vec(),
                };
                let count = params.get("count").and_then(Value::as_u64).unwrap_or(1) as usize;
                let parallel = params
                    .get("parallel")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let roots = relay_proxy::compose::roots_from_pem(&pem);
                let results =
                    relay_proxy::compose::send_many(addr, &roots, &spec, count, parallel).await;
                Ok(Value::Array(
                    results
                        .into_iter()
                        .map(|r| match r {
                            Ok(r) => json!({
                                "status": r.status,
                                "headers": r.headers.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>(),
                                "body_preview": r.body_preview,
                                "body_size": r.body_size,
                                "duration_ms": r.duration_ms,
                            }),
                            Err(e) => json!({"status": 0, "error": e}),
                        })
                        .collect(),
                ))
            }
            "traffic.clear" => {
                self.flush();
                self.store.clear().map_err(engine_err)?;
                Ok(json!({}))
            }
            "rules.get" => {
                let set = self.rules.lock().unwrap().clone();
                let problems: Vec<String> = set.validate().iter().map(|p| p.to_string()).collect();
                Ok(json!({ "yaml": set.to_yaml(), "problems": problems }))
            }
            "rules.set" => {
                let yaml = params
                    .get("yaml")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("`yaml` is required"))?;
                let set = match RuleSet::from_yaml(yaml) {
                    Ok(s) => s,
                    Err(e) => return Ok(json!({ "saved": false, "problems": [e] })),
                };
                let problems: Vec<String> = set.validate().iter().map(|p| p.to_string()).collect();
                if !problems.is_empty() {
                    return Ok(json!({ "saved": false, "problems": problems }));
                }
                write_atomic(&self.opts.data_dir.join(RULES_FILE), yaml.as_bytes())
                    .map_err(engine_err)?;
                if let Some(h) = self.proxy.lock().await.as_ref() {
                    h.set_rules(set.clone(), &self.opts.data_dir);
                }
                *self.rules.lock().unwrap() = set;
                Ok(json!({ "saved": true, "problems": [] }))
            }
            "settings.get" => Ok(self.settings_json()),
            "settings.set" => {
                // Validate everything first so a bad value changes nothing.
                let decrypt = match params.get("decrypt") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(
                        v.as_bool()
                            .ok_or_else(|| bad("`decrypt` is true or false"))?,
                    ),
                };
                let throttle = match params.get("throttle") {
                    None => None,
                    Some(Value::Null) => Some(None),
                    Some(v) => {
                        let p = v
                            .as_str()
                            .ok_or_else(|| bad("`throttle` is a preset name or null"))?;
                        if matches!(p, "" | "off" | "none") {
                            Some(None)
                        } else if relay_model::rules::THROTTLE_PRESETS.contains(&p) {
                            Some(Some(p.to_owned()))
                        } else {
                            return Err(bad(&format!(
                                "unknown throttle preset `{p}` (use one of {}, or null to turn it off)",
                                relay_model::rules::THROTTLE_PRESETS.join(", ")
                            )));
                        }
                    }
                };
                {
                    let mut sw = self.switches.lock().unwrap();
                    if let Some(d) = decrypt {
                        sw.decrypt = d;
                    }
                    if let Some(t) = throttle.clone() {
                        sw.throttle = t;
                    }
                }
                if let Some(h) = self.proxy.lock().await.as_ref() {
                    if let Some(d) = decrypt {
                        h.set_decrypt(d);
                    }
                    if let Some(t) = throttle {
                        h.set_global_throttle(t).map_err(|m| bad(&m))?;
                    }
                }
                Ok(self.settings_json())
            }
            "breakpoints.list" => Ok(json!(self.paused_json().await)),
            "breakpoints.wait" => {
                // Long-poll for scripts and agents: returns as soon as something is held.
                let wait = params
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .min(120_000);
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait);
                loop {
                    let held = self.paused_json().await;
                    if !held.is_empty() || tokio::time::Instant::now() >= deadline {
                        return Ok(json!(held));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            "breakpoints.resume" | "breakpoints.abort" => {
                let id = params
                    .get("tx_id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| bad("`tx_id` is required"))?;
                let held = self.paused().await;
                // Without `phase`, act on whichever phase of that transaction is held.
                let phase = match params.get("phase").and_then(Value::as_str) {
                    Some("request") => Some(BreakPhase::Request),
                    Some("response") => Some(BreakPhase::Response),
                    Some(_) => return Err(bad("`phase` is \"request\" or \"response\"")),
                    None => held.iter().find(|p| p.tx_id == id).map(|p| p.phase),
                };
                let item =
                    phase.and_then(|ph| held.iter().find(|p| p.tx_id == id && p.phase == ph));
                let (Some(phase), Some(item)) = (phase, item) else {
                    return Err(RpcError::new(
                        RpcError::ENGINE,
                        format!(
                            "nothing is held for transaction {id} (already released, or timed out)"
                        ),
                    ));
                };
                let guard = self.proxy.lock().await;
                let handle = guard
                    .as_ref()
                    .ok_or_else(|| RpcError::new(RpcError::ENGINE, "the proxy is not running"))?;
                let done = if method == "breakpoints.abort" {
                    handle.abort(id, phase)
                } else {
                    let edit = match params.get("edit") {
                        None | Some(Value::Null) => None,
                        Some(e) => Some(edit_from(e, item).map_err(|m| bad(&m))?),
                    };
                    handle.resume(id, phase, edit)
                };
                Ok(json!({ "done": done }))
            }
            "devices.list" => {
                let devices = self.devices.clone();
                let list = tokio::task::spawn_blocking(move || devices.list())
                    .await
                    .map_err(|e| engine_err(e.into()))?;
                let port = self.proxy_port().await.unwrap_or(8888);
                let wifi = relay_devices::wifi_setup_url(self.devices_runner.as_ref(), port);
                Ok(
                    json!({ "devices": list, "wifi_setup_url": wifi, "proxy_port": self.proxy_port().await }),
                )
            }
            "devices.connect" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("`id` is required"))?
                    .to_string();
                let port = self
                    .proxy_port()
                    .await
                    .ok_or_else(|| RpcError::new(RpcError::ENGINE, "start the proxy first"))?;
                let devices = self.devices.clone();
                let r = tokio::task::spawn_blocking(move || devices.connect(&id, port))
                    .await
                    .map_err(|e| engine_err(e.into()))?
                    .map_err(engine_err)?;
                Ok(
                    json!({ "device": r.device, "checklist": r.checklist, "ready": r.checklist.ready(), "connected_at_ms": r.connected_at_ms }),
                )
            }
            "devices.disconnect" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("`id` is required"))?
                    .to_string();
                let devices = self.devices.clone();
                tokio::task::spawn_blocking(move || devices.disconnect(&id))
                    .await
                    .map_err(|e| engine_err(e.into()))?
                    .map_err(engine_err)?;
                Ok(json!({}))
            }
            "ca.info" => {
                let path = self.opts.data_dir.join(relay_proxy::ca::CA_CERT_FILE);
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let ca =
                    relay_proxy::ca::CertAuthority::load_or_create(&self.opts.data_dir, provider)
                        .map_err(engine_err)?;
                Ok(json!({ "path": path, "pem": ca.ca_pem() }))
            }
            "system_proxy.status" => Ok(self.system_proxy_required()?.status_json()),
            "system_proxy.enable" => {
                let sp = self.system_proxy_required()?;
                let port = match params.get("port").and_then(Value::as_u64) {
                    Some(p) => p as u16,
                    None => self
                        .proxy_port()
                        .await
                        .ok_or_else(|| RpcError::new(RpcError::ENGINE, "start the proxy first"))?,
                };
                sp.enable("127.0.0.1", port).map_err(engine_err)?;
                Ok(sp.status_json())
            }
            "system_proxy.disable" => {
                let sp = self.system_proxy_required()?;
                sp.disable().map_err(engine_err)?;
                Ok(sp.status_json())
            }
            other => Err(RpcError::new(
                RpcError::METHOD_NOT_FOUND,
                format!("unknown method `{other}`"),
            )),
        }
    }

    fn system_proxy_required(&self) -> Result<&SystemProxy, RpcError> {
        self.system_proxy.as_deref().ok_or_else(|| {
            RpcError::new(
                RpcError::ENGINE,
                "system proxy control is not available on this platform",
            )
        })
    }
}

fn load_rules(path: &Path) -> Result<RuleSet> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            RuleSet::from_yaml(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RuleSet {
            version: RULES_VERSION,
            rules: vec![],
        }),
        Err(e) => Err(e.into()),
    }
}

/// Writes via a temp file + rename so a crash never leaves a half-written file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Builds the edited message from the API's `edit` object. Anything left out keeps its held value:
/// `{"status": 503}`, `{"set_headers": {"x-a": "b"}}` (add/replace), `{"remove_headers": ["x-c"]}`,
/// `{"headers": [{name,value}…]}` (replace all), `{"body": "text"}`.
fn edit_from(e: &Value, held: &Paused) -> Result<Edit, String> {
    let mut headers = held.headers.clone();
    if let Some(all) = e.get("headers") {
        headers = all
            .as_array()
            .ok_or("`headers` must be a list of {name, value}")?
            .iter()
            .map(|h| {
                Some(relay_model::Header {
                    name: h.get("name")?.as_str()?.to_owned(),
                    value: h.get("value")?.as_str()?.to_owned(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("every header needs a string `name` and `value`")?;
    }
    if let Some(names) = e.get("remove_headers").and_then(Value::as_array) {
        for n in names.iter().filter_map(Value::as_str) {
            headers.retain(|h| !h.name.eq_ignore_ascii_case(n));
        }
    }
    if let Some(set) = e.get("set_headers").and_then(Value::as_object) {
        for (name, value) in set {
            let value = value
                .as_str()
                .ok_or("`set_headers` values must be strings")?;
            headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
            headers.push(relay_model::Header {
                name: name.clone(),
                value: value.to_owned(),
            });
        }
    }
    let body = match e.get("body") {
        None | Some(Value::Null) => held.body.clone(),
        Some(b) => b
            .as_str()
            .ok_or("`body` must be a string")?
            .as_bytes()
            .to_vec(),
    };
    let status = match e.get("status") {
        None | Some(Value::Null) => held.status,
        Some(v) => Some(
            v.as_u64()
                .filter(|s| (100..600).contains(s))
                .ok_or("`status` must be 100-599")? as u16,
        ),
    };
    Ok(Edit {
        status,
        headers,
        body,
    })
}

impl Engine {
    async fn paused(&self) -> Vec<Paused> {
        self.proxy
            .lock()
            .await
            .as_ref()
            .map(|h| h.paused())
            .unwrap_or_default()
    }

    async fn paused_json(&self) -> Vec<Value> {
        self.paused()
            .await
            .into_iter()
            .map(|p| {
                let text = std::str::from_utf8(&p.body).ok();
                json!({
                    "tx_id": p.tx_id,
                    "phase": if p.phase == BreakPhase::Request { "request" } else { "response" },
                    "rule": p.rule,
                    "method": p.method,
                    "url": p.url,
                    "status": p.status,
                    "headers": p.headers,
                    "body": text.map(str::to_owned).unwrap_or_else(|| String::from_utf8_lossy(&p.body).into_owned()),
                    "body_is_text": text.is_some(),
                    "body_size": p.body.len(),
                })
            })
            .collect()
    }
}

impl Engine {
    fn settings_json(&self) -> Value {
        let sw = self.switches.lock().unwrap().clone();
        json!({
            "decrypt": sw.decrypt,
            "throttle": sw.throttle,
            "throttle_presets": relay_model::rules::THROTTLE_PRESETS,
        })
    }
}
