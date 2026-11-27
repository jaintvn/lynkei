//! The control API, served from inside the Mac app.
//!
//! `lynkeid` answers JSON-RPC lines on a Unix socket; the MCP server (`lynkei-mcp`) is a client of
//! it. This module answers the same calls from the app's own engine, so an agent sees exactly what
//! the app sees: the same live traffic, rules, devices and held requests, on the same proxy port.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{FfiComposeRequest, FfiEdit, FfiHeader, LynkeiEngine, LynkeiError};

type Reply = Result<Value, (i64, String)>;

const INVALID_PARAMS: i64 = -32602;
const ENGINE: i64 = -32000;

fn bad(m: impl Into<String>) -> (i64, String) {
    (INVALID_PARAMS, m.into())
}
fn fail(m: impl Into<String>) -> (i64, String) {
    (ENGINE, m.into())
}
fn ffi(e: LynkeiError) -> (i64, String) {
    fail(e.to_string())
}

#[uniffi::export]
impl LynkeiEngine {
    /// Starts answering the control API on Lynkei's control socket (replacing a stale one), so
    /// `lynkei-mcp` can attach to this app. Returns the socket path. Call once per run.
    pub fn start_agent_api(self: Arc<Self>) -> Result<String, LynkeiError> {
        let path = lynkei_control::default_socket(&self.data_dir);
        let socket_path = path.to_string_lossy().into_owned();
        let _guard = self.runtime.enter();
        let listener = lynkei_control::server::bind(&path).map_err(|e| LynkeiError::Failed {
            reason: format!("{e:#}"),
        })?;
        let engine = self.clone();
        self.runtime.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let engine = engine.clone();
                tokio::spawn(async move {
                    let _ = connection(stream, engine).await;
                });
            }
        });
        Ok(socket_path)
    }
}

async fn connection(
    stream: tokio::net::UnixStream,
    engine: Arc<LynkeiEngine>,
) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Value>(256);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if w.write_all(format!("{msg}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let engine = engine.clone();
        let out = out_tx.clone();
        tokio::spawn(async move {
            let msg = match handle(engine, &method, params).await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                }
            };
            let _ = out.send(msg).await;
        });
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// Runs a blocking engine call (device tools, compose) off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, (i64, String)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.map_err(|_| fail("the engine call was dropped"))
}

fn text<'a>(p: &'a Value, k: &str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or("")
}

fn proxy_port(e: &LynkeiEngine) -> Option<u16> {
    e.handle
        .lock()
        .unwrap()
        .as_ref()
        .map(|h| h.local_addr.port())
}

fn settings(e: &LynkeiEngine) -> Value {
    json!({
        "decrypt": e.decrypt_enabled(),
        "throttle": e.global_throttle(),
        "throttle_presets": e.throttle_presets(),
    })
}

fn paused_json(e: &LynkeiEngine) -> Vec<Value> {
    e.paused()
        .into_iter()
        .map(|p| {
            json!({
                "tx_id": p.tx_id,
                "phase": if p.response { "response" } else { "request" },
                "rule": p.rule,
                "method": p.method,
                "url": p.url,
                "status": p.status,
                "headers": p.headers.iter().map(|h| json!({"name": h.name, "value": h.value})).collect::<Vec<_>>(),
                "body_size": p.body.len(),
                "body": p.body,
                "body_is_text": p.body_is_text,
            })
        })
        .collect()
}

fn edit_from(e: &Value, held: &crate::FfiPaused) -> Result<FfiEdit, String> {
    let mut headers: Vec<FfiHeader> = held.headers.clone();
    if let Some(all) = e.get("headers") {
        headers = all
            .as_array()
            .ok_or("`headers` must be a list of {name, value}")?
            .iter()
            .map(|h| {
                Some(FfiHeader {
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
            headers.push(FfiHeader {
                name: name.clone(),
                value: value.to_owned(),
            });
        }
    }
    let body = match e.get("body") {
        None | Some(Value::Null) => held.body.clone(),
        Some(b) => b.as_str().ok_or("`body` must be a string")?.to_owned(),
    };
    let status = match e.get("status") {
        None | Some(Value::Null) => held.status,
        Some(v) => Some(
            v.as_u64()
                .filter(|s| (100..600).contains(s))
                .ok_or("`status` must be 100-599")? as u16,
        ),
    };
    Ok(FfiEdit {
        status,
        headers,
        body,
    })
}

async fn handle(e: Arc<LynkeiEngine>, method: &str, params: Value) -> Reply {
    match method {
        "hello" => Ok(json!({
            "api": lynkei_control::API_VERSION,
            "engine_version": env!("CARGO_PKG_VERSION"),
            "served_by": "Lynkei app",
            "proxy": { "running": proxy_port(&e).is_some(), "port": proxy_port(&e) },
            "store": { "transactions": e.stored_count(), "dropped": 0 },
            "ca_path": e.ca_certificate_path().ok(),
            "rules": e.list_rules().len(),
        })),
        "proxy.start" => {
            // The app owns the proxy; report where it is.
            match proxy_port(&e) {
                Some(port) => Ok(json!({ "port": port })),
                None => Err(fail("the Lynkei app has not started its proxy yet")),
            }
        }
        "proxy.stop" => Err(fail(
            "the proxy belongs to the Lynkei app; stop it there (or use `set_ssl_decrypt` / rules)",
        )),
        "traffic.search" => {
            let store = e
                .store
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| fail("the proxy has not started yet"))?;
            let filter = lynkei_model::filter::Filter::parse(text(&params, "query"))
                .map_err(|x| bad(x.to_string()))?;
            let limit = params
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(10_000) as usize;
            let rows = store
                .search(&filter, limit)
                .map_err(|x| fail(x.to_string()))?;
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
            let store = e
                .store
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| fail("the proxy has not started yet"))?;
            match store.get(key).map_err(|x| fail(x.to_string()))? {
                Some(r) => Ok(json!({"key": r.key, "session": r.session, "transaction": r.tx})),
                None => Err(fail(format!("no transaction {key}"))),
            }
        }
        "traffic.clear" => {
            let store = e.store.lock().unwrap().clone();
            if let Some(s) = store {
                s.clear().map_err(|x| fail(x.to_string()))?;
            }
            Ok(json!({}))
        }
        "compose.send" => {
            let url = text(&params, "url").to_string();
            if url.is_empty() {
                return Err(bad("`url` is required"));
            }
            let m = match text(&params, "method") {
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
                            headers.push(FfiHeader {
                                name: n.into(),
                                value: v.into(),
                            });
                        }
                    }
                }
                Some(Value::Object(o)) => {
                    for (n, v) in o {
                        if let Some(v) = v.as_str() {
                            headers.push(FfiHeader {
                                name: n.clone(),
                                value: v.into(),
                            });
                        }
                    }
                }
                _ => {}
            }
            let req = FfiComposeRequest {
                method: m,
                url,
                headers,
                body: text(&params, "body").to_string(),
            };
            let count = params.get("count").and_then(Value::as_u64).unwrap_or(1) as u32;
            let parallel = params
                .get("parallel")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let engine = e.clone();
            let results = blocking(move || engine.compose(req, count, parallel))
                .await?
                .map_err(ffi)?;
            Ok(Value::Array(
                results
                    .into_iter()
                    .map(|r| match r.error {
                        Some(err) => json!({"status": 0, "error": err}),
                        None => json!({
                            "status": r.status,
                            "headers": r.headers.iter().map(|h| json!({"name": h.name, "value": h.value})).collect::<Vec<_>>(),
                            "body_preview": r.body_preview,
                            "body_size": r.body_size,
                            "duration_ms": r.duration_ms,
                        }),
                    })
                    .collect(),
            ))
        }
        "rules.get" => {
            let yaml = e.rules_yaml();
            let problems = lynkei_model::rules::RuleSet::from_yaml(&yaml)
                .map(|s| {
                    s.validate()
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(json!({ "yaml": yaml, "problems": problems }))
        }
        "rules.set" => {
            let yaml = params
                .get("yaml")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("`yaml` is required"))?;
            let problems = e.set_rules(yaml.to_string());
            Ok(json!({ "saved": problems.is_empty(), "problems": problems }))
        }
        "settings.get" => Ok(settings(&e)),
        "settings.set" => {
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
                    } else if e.throttle_presets().iter().any(|x| x == p) {
                        Some(Some(p.to_owned()))
                    } else {
                        return Err(bad(format!(
                            "unknown throttle preset `{p}` (use one of {}, or null to turn it off)",
                            e.throttle_presets().join(", ")
                        )));
                    }
                }
            };
            if let Some(d) = decrypt {
                e.set_decrypt(d);
            }
            if let Some(t) = throttle {
                e.set_global_throttle(t).map_err(ffi)?;
            }
            Ok(settings(&e))
        }
        "breakpoints.list" => Ok(json!(paused_json(&e))),
        "breakpoints.wait" => {
            let wait = params
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(30_000)
                .min(120_000);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait);
            loop {
                let held = paused_json(&e);
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
            let held = e.paused();
            let response = match params.get("phase").and_then(Value::as_str) {
                Some("request") => Some(false),
                Some("response") => Some(true),
                Some(_) => return Err(bad("`phase` is \"request\" or \"response\"")),
                None => held.iter().find(|p| p.tx_id == id).map(|p| p.response),
            };
            let item =
                response.and_then(|r| held.iter().find(|p| p.tx_id == id && p.response == r));
            let (Some(response), Some(item)) = (response, item) else {
                return Err(fail(format!(
                    "nothing is held for transaction {id} (already released, or timed out)"
                )));
            };
            let done = if method == "breakpoints.abort" {
                e.abort_paused(id, response)
            } else {
                let edit = match params.get("edit") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(edit_from(v, item).map_err(bad)?),
                };
                e.resume_paused(id, response, edit)
            };
            Ok(json!({ "done": done }))
        }
        "devices.list" => {
            let devices = e.devices.clone();
            let list = blocking(move || devices.list()).await?;
            let port = proxy_port(&e).unwrap_or(8888);
            Ok(json!({
                "devices": list,
                "wifi_setup_url": e.wifi_setup_url(),
                "proxy_port": port,
            }))
        }
        "devices.connect" => {
            let id = text(&params, "id").to_string();
            if id.is_empty() {
                return Err(bad("`id` is required"));
            }
            let port = proxy_port(&e).ok_or_else(|| fail("the proxy has not started yet"))?;
            let devices = e.devices.clone();
            let r = blocking(move || devices.connect(&id, port))
                .await?
                .map_err(|x| fail(format!("{x:#}")))?;
            Ok(json!({
                "device": r.device,
                "checklist": r.checklist,
                "ready": r.checklist.ready(),
                "connected_at_ms": r.connected_at_ms,
            }))
        }
        "devices.proxy" => {
            let id = text(&params, "id").to_string();
            if id.is_empty() {
                return Err(bad("`id` is required"));
            }
            let enabled = params
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| bad("`enabled` (true or false) is required"))?;
            let port = proxy_port(&e).ok_or_else(|| fail("the proxy has not started yet"))?;
            let devices = e.devices.clone();
            let r = blocking(move || devices.set_proxy(&id, enabled, port))
                .await?
                .map_err(|x| fail(format!("{x:#}")))?;
            Ok(json!({
                "device": r.device,
                "checklist": r.checklist,
                "ready": r.checklist.ready(),
                "connected_at_ms": r.connected_at_ms,
            }))
        }
        "devices.disconnect" => {
            let id = text(&params, "id").to_string();
            if id.is_empty() {
                return Err(bad("`id` is required"));
            }
            let devices = e.devices.clone();
            blocking(move || devices.disconnect(&id))
                .await?
                .map_err(|x| fail(format!("{x:#}")))?;
            Ok(json!({}))
        }
        "ca.info" => {
            let path = e.ca_certificate_path().map_err(ffi)?;
            let pem = e.ca_certificate_pem().map_err(ffi)?;
            Ok(json!({ "path": path, "pem": pem }))
        }
        other => Err((-32601, format!("unknown method `{other}`"))),
    }
}
