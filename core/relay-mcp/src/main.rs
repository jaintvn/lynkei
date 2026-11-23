//! relay-mcp: a Model Context Protocol server (stdio) that lets an AI agent drive Relay.
//!
//! It is a thin shim over the control API of `relayd` (see `relay-control`), so an agent can do
//! what the Mac app does: connect a device, search traffic, add a mock or rewrite rule, and handle
//! breakpoints. Register it with an agent as a stdio server: `relay-mcp` (it finds `relayd`'s
//! socket in `$RELAY_DATA_DIR`, or the default data directory; `--socket <path>` overrides).
//!
//! Safety: `Authorization`, cookies and API-key headers are redacted from traffic unless the
//! caller asks for `include_secrets`, and bodies are left out unless `include_bodies` is set.

use std::path::PathBuf;

use anyhow::Result;
use relay_model::rules::{Rule, RuleSet};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL: &str = "2024-11-05";
const MAX_TEXT: usize = 20_000;

fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("RELAY_DATA_DIR") {
        return d.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Relay")
    } else {
        PathBuf::from(home).join(".relay")
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let socket = match opt("--socket") {
        Some(s) => PathBuf::from(s),
        None => relay_control::default_socket(
            &opt("--data-dir")
                .map(PathBuf::from)
                .unwrap_or_else(default_data_dir),
        ),
    };

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, &socket).await,
            Err(e) => Some(error_reply(
                Value::Null,
                -32700,
                &format!("parse error: {e}"),
            )),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// One incoming message; `None` for notifications (no reply).
async fn handle(msg: &Value, socket: &std::path::Path) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = match id {
        Some(i) => i,
        None => return None, // notification, such as notifications/initialized
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "relay", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Relay is a debugging proxy. Start with `status`; connect a device with `list_devices` and `connect_device`; read traffic with `search_traffic`; change behaviour with `add_rule`; handle held calls with `list_paused` and `resume_paused`."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(name, &args, socket).await {
                Ok(v) => {
                    json!({"content": [{"type": "text", "text": text_of(&v)}], "isError": false})
                }
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        other => {
            return Some(error_reply(
                id,
                -32601,
                &format!("method not found: {other}"),
            ));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn text_of(v: &Value) -> String {
    let s = serde_json::to_string_pretty(v).unwrap_or_default();
    if s.len() > 200_000 {
        format!("{}\n…[truncated]", &s[..200_000])
    } else {
        s
    }
}

async fn rpc(socket: &std::path::Path, method: &str, params: Value) -> Result<Value, String> {
    let mut c = relay_control::Client::connect(socket)
        .await
        .map_err(|e| format!("{e:#}"))?;
    c.call(method, params).await.map_err(|e| format!("{e:#}"))
}

fn tool(
    name: &str,
    description: &str,
    props: Value,
    required: &[&str],
    destructive: bool,
) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": props, "required": required},
        "annotations": {"destructiveHint": destructive, "readOnlyHint": false}
    })
}

fn tools() -> Vec<Value> {
    let s = |d: &str| json!({"type": "string", "description": d});
    let b = |d: &str| json!({"type": "boolean", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    vec![
        tool(
            "status",
            "Relay's state: proxy, store, CA and system proxy.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "proxy_start",
            "Start the proxy (default port 8888).",
            json!({"port": n("listen port")}),
            &[],
            false,
        ),
        tool("proxy_stop", "Stop the proxy.", json!({}), &[], false),
        tool(
            "list_devices",
            "Simulators, emulators and phones Relay can see, and whether each is connected.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "connect_device",
            "Point a device at the proxy and install the Relay certificate. Returns a checklist.",
            json!({"id": s("device id from list_devices")}),
            &["id"],
            false,
        ),
        tool(
            "disconnect_device",
            "Undo the connection for a device.",
            json!({"id": s("device id")}),
            &["id"],
            false,
        ),
        tool(
            "search_traffic",
            "Search captured calls, newest first. Query language: `status:5xx host:api.acme.com method:POST path:/v2/* duration:>800ms body:\"text\" app:Name header:name=value`, bare words, `-` to negate.",
            json!({"query": s("search query; empty for everything"), "limit": n("max results, default 20"), "include_secrets": b("keep Authorization, cookies and API keys in headers")}),
            &[],
            false,
        ),
        tool(
            "get_transaction",
            "One captured call with headers (and bodies when asked).",
            json!({"key": s("key from search_traffic"), "include_bodies": b("include request and response bodies (long)"), "include_secrets": b("keep Authorization, cookies and API keys in headers")}),
            &["key"],
            false,
        ),
        tool(
            "clear_traffic",
            "Delete all captured calls.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "list_rules",
            "The rules as YAML, with any problems.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "add_rule",
            "Add one rule. `rule` is an object as in the rules YAML: id, kind (rewrite, map_local, map_remote, breakpoint, block, throttle, dns_spoof), match {method, host, path}, and the kind's fields (for example actions for rewrite). Replaces a rule with the same id.",
            json!({"rule": {"type": "object", "description": "the rule"}}),
            &["rule"],
            false,
        ),
        tool(
            "toggle_rule",
            "Switch a rule on or off.",
            json!({"id": s("rule id"), "enabled": b("true for on")}),
            &["id", "enabled"],
            false,
        ),
        tool(
            "delete_rule",
            "Remove a rule.",
            json!({"id": s("rule id")}),
            &["id"],
            true,
        ),
        tool(
            "get_settings",
            "HTTPS decryption and the network-slowdown preset.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "set_ssl_decrypt",
            "Turn HTTPS decryption on or off (off: HTTPS passes through untouched).",
            json!({"enabled": b("true to decrypt")}),
            &["enabled"],
            false,
        ),
        tool(
            "set_throttle",
            "Slow every request with a preset (3g, edge, lte, lte-lossy, high-latency, offline), or \"off\".",
            json!({"preset": s("preset name or off")}),
            &["preset"],
            false,
        ),
        tool(
            "list_paused",
            "Requests and responses held at a pause (breakpoint) rule.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "wait_paused",
            "Wait until something is held (up to 120 s) and return it.",
            json!({"timeout_ms": n("how long to wait")}),
            &[],
            false,
        ),
        tool(
            "resume_paused",
            "Let a held call continue, optionally changed. `edit` may set status, headers, set_headers, remove_headers, body.",
            json!({"tx_id": n("tx_id from list_paused"), "phase": s("request or response"), "edit": {"type": "object", "description": "changes; leave out to continue unchanged"}}),
            &["tx_id"],
            false,
        ),
        tool(
            "abort_paused",
            "Fail a held call (the app gets an error).",
            json!({"tx_id": n("tx_id"), "phase": s("request or response")}),
            &["tx_id"],
            true,
        ),
        tool(
            "ca_info",
            "The path and PEM of Relay's certificate authority.",
            json!({}),
            &[],
            false,
        ),
    ]
}

const SECRET_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
];

/// Hides secret header values, drops bodies unless asked, and keeps long text short.
fn slim(v: &mut Value, bodies: bool, secrets: bool) {
    match v {
        Value::Object(m) => {
            if !secrets
                && let (Some(Value::String(name)), true) = (m.get("name"), m.contains_key("value"))
                && SECRET_HEADERS.contains(&name.to_lowercase().as_str())
            {
                m.insert("value".into(), json!("[redacted]"));
            }
            if !bodies {
                m.retain(|k, _| !k.ends_with("_body_preview"));
            }
            for x in m.values_mut() {
                slim(x, bodies, secrets);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| slim(x, bodies, secrets)),
        Value::String(s) if s.len() > MAX_TEXT => {
            let mut end = MAX_TEXT;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push_str("…[truncated]");
        }
        _ => {}
    }
}

fn flag(a: &Value, k: &str) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn req<'a>(a: &'a Value, k: &str) -> Result<&'a Value, String> {
    a.get(k)
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("`{k}` is required"))
}

async fn call_tool(name: &str, a: &Value, socket: &std::path::Path) -> Result<Value, String> {
    let pass = |keys: &[&str]| -> Value {
        let mut m = serde_json::Map::new();
        for k in keys {
            if let Some(v) = a.get(*k) {
                m.insert((*k).into(), v.clone());
            }
        }
        Value::Object(m)
    };
    match name {
        "status" => rpc(socket, "hello", Value::Null).await,
        "proxy_start" => rpc(socket, "proxy.start", pass(&["port"])).await,
        "proxy_stop" => rpc(socket, "proxy.stop", Value::Null).await,
        "list_devices" => rpc(socket, "devices.list", Value::Null).await,
        "connect_device" => rpc(socket, "devices.connect", json!({"id": req(a, "id")?})).await,
        "disconnect_device" => {
            rpc(socket, "devices.disconnect", json!({"id": req(a, "id")?})).await
        }
        "search_traffic" => {
            let limit = a
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .min(200);
            let mut r = rpc(socket, "traffic.search", json!({"query": a.get("query").and_then(Value::as_str).unwrap_or(""), "limit": limit})).await?;
            slim(&mut r, false, flag(a, "include_secrets"));
            Ok(r)
        }
        "get_transaction" => {
            let mut r = rpc(socket, "traffic.get", json!({"key": req(a, "key")?})).await?;
            slim(
                &mut r,
                flag(a, "include_bodies"),
                flag(a, "include_secrets"),
            );
            Ok(r)
        }
        "clear_traffic" => rpc(socket, "traffic.clear", Value::Null).await,
        "list_rules" => rpc(socket, "rules.get", Value::Null).await,
        "add_rule" | "toggle_rule" | "delete_rule" => edit_rules(name, a, socket).await,
        "get_settings" => rpc(socket, "settings.get", Value::Null).await,
        "set_ssl_decrypt" => {
            rpc(
                socket,
                "settings.set",
                json!({"decrypt": req(a, "enabled")?}),
            )
            .await
        }
        "set_throttle" => {
            let p = req(a, "preset")?.as_str().unwrap_or("off");
            let v = if p == "off" { Value::Null } else { json!(p) };
            rpc(socket, "settings.set", json!({"throttle": v})).await
        }
        "list_paused" => rpc(socket, "breakpoints.list", Value::Null).await,
        "wait_paused" => rpc(socket, "breakpoints.wait", pass(&["timeout_ms"])).await,
        "resume_paused" => {
            rpc(
                socket,
                "breakpoints.resume",
                pass(&["tx_id", "phase", "edit"]),
            )
            .await
        }
        "abort_paused" => rpc(socket, "breakpoints.abort", pass(&["tx_id", "phase"])).await,
        "ca_info" => rpc(socket, "ca.info", Value::Null).await,
        other => Err(format!("unknown tool: {other}")),
    }
}

/// add / toggle / delete work on the whole rules file: read it, change it, validate and save it.
async fn edit_rules(name: &str, a: &Value, socket: &std::path::Path) -> Result<Value, String> {
    let current = rpc(socket, "rules.get", Value::Null).await?;
    let yaml = current.get("yaml").and_then(Value::as_str).unwrap_or("");
    let mut set = if yaml.trim().is_empty() {
        RuleSet::from_yaml("version: 1\nrules: []\n")?
    } else {
        RuleSet::from_yaml(yaml)?
    };
    match name {
        "add_rule" => {
            let rule: Rule = serde_json::from_value(req(a, "rule")?.clone())
                .map_err(|e| format!("not a valid rule: {e}"))?;
            set.rules.retain(|r| r.id != rule.id);
            set.rules.push(rule);
        }
        "toggle_rule" => {
            let id = req(a, "id")?.as_str().unwrap_or("");
            let on = req(a, "enabled")?.as_bool().unwrap_or(true);
            let r = set
                .rules
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| format!("no rule `{id}`"))?;
            r.enabled = on;
        }
        _ => {
            let id = req(a, "id")?.as_str().unwrap_or("");
            let before = set.rules.len();
            set.rules.retain(|r| r.id != id);
            if set.rules.len() == before {
                return Err(format!("no rule `{id}`"));
            }
        }
    }
    rpc(socket, "rules.set", json!({"yaml": set.to_yaml()})).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_and_list_tools() {
        let sock = std::path::Path::new("/nonexistent");
        let r = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}), sock).await.unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "relay");
        assert!(
            handle(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                sock
            )
            .await
            .is_none()
        );
        let t = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), sock)
            .await
            .unwrap();
        let names: Vec<&str> = t["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["name"].as_str())
            .collect();
        assert!(
            names.contains(&"search_traffic")
                && names.contains(&"resume_paused")
                && names.contains(&"add_rule")
        );
    }

    #[tokio::test]
    async fn unreachable_daemon_is_a_tool_error_not_a_crash() {
        let r = handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"status","arguments":{}}}), std::path::Path::new("/nonexistent/relayd.sock")).await.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("relayd")
        );
    }

    #[test]
    fn secrets_and_bodies_are_hidden_by_default() {
        let mut v = json!({"request_headers":[{"name":"Authorization","value":"Bearer x"},{"name":"accept","value":"*/*"}],"response_body_preview":"{\"a\":1}"});
        slim(&mut v, false, false);
        assert_eq!(v["request_headers"][0]["value"], "[redacted]");
        assert_eq!(v["request_headers"][1]["value"], "*/*");
        assert!(v.get("response_body_preview").is_none());
        let mut w = json!({"response_body_preview":"ok","h":[{"name":"cookie","value":"s"}]});
        slim(&mut w, true, true);
        assert_eq!(w["response_body_preview"], "ok");
        assert_eq!(w["h"][0]["value"], "s");
    }
}
