//! The engine relayd hosts: proxy + store + rules + live events + system proxy, behind one API.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use relay_model::Transaction;
use relay_model::filter::Filter;
use relay_model::rules::{RULES_VERSION, RuleSet};
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
}

impl EngineOptions {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            extra_upstream_roots: vec![],
            store: StoreConfig::default(),
            system_proxy: None,
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
    system_proxy: Option<SystemProxy>,
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
            Some(backend) => Some(SystemProxy::new(backend.clone(), &opts.data_dir)?),
            None => None,
        };
        Ok(Arc::new(Engine {
            opts,
            store,
            sink,
            events,
            proxy: Mutex::new(None),
            rules: std::sync::Mutex::new(rules),
            system_proxy,
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
        self.system_proxy.as_ref()
    }

    /// Restores system settings Relay changed. Call on shutdown.
    pub async fn shutdown(&self) {
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
                *self.rules.lock().unwrap() = set;
                Ok(json!({ "saved": true, "problems": [] }))
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
        self.system_proxy.as_ref().ok_or_else(|| {
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
