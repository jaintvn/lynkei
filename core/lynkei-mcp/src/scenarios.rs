//! Scenarios: calls recorded from traffic, saved under the data directory, and replayed as mock
//! rules. Saved as `scenarios/<name>/scenario.json` plus one body file per call.

use std::path::PathBuf;

use lynkei_model::Transaction;
use lynkei_model::rules::{Rule, RuleSet};
use serde_json::{Value, json};

use crate::{Ctx, flag, req, rpc, slim};

const TRUNCATED: &str = "…[truncated]";
const FRAMING: [&str; 6] = [
    "content-length",
    "transfer-encoding",
    "content-encoding",
    "connection",
    "keep-alive",
    "date",
];

fn root(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("scenarios")
}

fn name_of(a: &Value) -> Result<String, String> {
    let n = req(a, "name")?.as_str().unwrap_or("").trim().to_string();
    if n.is_empty()
        || n.len() > 64
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("`name` must be 1 to 64 letters, digits, - or _".into());
    }
    Ok(n)
}

fn rule_prefix(name: &str) -> String {
    format!("scn-{name}-")
}

pub async fn record(a: &Value, ctx: &Ctx) -> Result<Value, String> {
    let name = name_of(a)?;
    let query = a.get("query").and_then(Value::as_str).unwrap_or("");
    let limit = a
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(100);
    let rows = rpc(
        &ctx.socket,
        "traffic.search",
        json!({"query": query, "limit": limit}),
    )
    .await?;
    let dir = root(ctx).join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut exchanges = Vec::new();
    let mut skipped = Vec::new();
    for row in rows.as_array().cloned().unwrap_or_default() {
        let Ok(t) = serde_json::from_value::<Transaction>(row["transaction"].clone()) else {
            continue;
        };
        let path = t.path.split('?').next().unwrap_or("/").to_string();
        let label = format!("{} {}{}", t.method, t.host, path);
        let Some(status) = t
            .status
            .filter(|_| t.state == lynkei_model::TransactionState::Complete)
        else {
            skipped.push(format!("{label}: no response"));
            continue;
        };
        if !seen.insert((t.method.clone(), t.host.clone(), path.clone())) {
            continue; // newest first: keep the newest answer for each endpoint
        }
        let body = match (&t.response_body_preview, t.response_body_size) {
            (Some(b), _) if b.ends_with(TRUNCATED) => {
                skipped.push(format!("{label}: body too large"));
                continue;
            }
            (Some(b), _) => b.clone(),
            (None, 0) => String::new(),
            (None, _) => {
                skipped.push(format!("{label}: binary body"));
                continue;
            }
        };
        let n = exchanges.len();
        std::fs::write(dir.join(format!("{n}.body")), body).map_err(|e| e.to_string())?;
        let headers: serde_json::Map<String, Value> = t
            .response_headers
            .iter()
            .filter(|h| !FRAMING.contains(&h.name.to_lowercase().as_str()))
            .map(|h| (h.name.clone(), json!(h.value)))
            .collect();
        exchanges.push(json!({"method": t.method, "host": t.host, "path": path, "status": status, "headers": headers, "file": format!("{n}.body")}));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let doc = json!({"name": name, "recorded_at_ms": now, "exchanges": exchanges});
    std::fs::write(
        dir.join("scenario.json"),
        serde_json::to_string_pretty(&doc).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"name": name, "recorded": exchanges.len(), "skipped": skipped, "next": "replay_scenario to answer these calls from the recording"}),
    )
}

pub fn list(ctx: &Ctx) -> Result<Value, String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root(ctx)) {
        for e in rd.flatten() {
            if let Ok(text) = std::fs::read_to_string(e.path().join("scenario.json"))
                && let Ok(v) = serde_json::from_str::<Value>(&text)
            {
                out.push(json!({"name": v["name"], "calls": v["exchanges"].as_array().map(Vec::len).unwrap_or(0), "recorded_at_ms": v["recorded_at_ms"]}));
            }
        }
    }
    Ok(json!({"scenarios": out}))
}

async fn rules_without(ctx: &Ctx, prefix: &str) -> Result<RuleSet, String> {
    let current = rpc(&ctx.socket, "rules.get", Value::Null).await?;
    let yaml = current.get("yaml").and_then(Value::as_str).unwrap_or("");
    let mut set = if yaml.trim().is_empty() {
        RuleSet::from_yaml("version: 1\nrules: []\n")?
    } else {
        RuleSet::from_yaml(yaml)?
    };
    set.rules.retain(|r| !r.id.starts_with(prefix));
    Ok(set)
}

pub async fn replay(a: &Value, ctx: &Ctx) -> Result<Value, String> {
    let name = name_of(a)?;
    let text = std::fs::read_to_string(root(ctx).join(&name).join("scenario.json"))
        .map_err(|_| format!("no scenario `{name}`"))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut set = rules_without(ctx, &rule_prefix(&name)).await?;
    let exchanges = doc["exchanges"].as_array().cloned().unwrap_or_default();
    for (i, x) in exchanges.iter().enumerate() {
        let rule = json!({
            "id": format!("{}{i}", rule_prefix(&name)),
            "name": format!("Scenario {name}: {} {}", x["method"].as_str().unwrap_or(""), x["path"].as_str().unwrap_or("")),
            "kind": "map_local",
            "match": {"method": x["method"], "host": x["host"], "path": x["path"]},
            "file": format!("scenarios/{name}/{}", x["file"].as_str().unwrap_or("")),
            "status": x["status"],
            "headers": x["headers"],
        });
        set.rules.push(
            serde_json::from_value::<Rule>(rule)
                .map_err(|e| format!("bad rule from scenario: {e}"))?,
        );
    }
    let saved = rpc(&ctx.socket, "rules.set", json!({"yaml": set.to_yaml()})).await?;
    Ok(
        json!({"name": name, "rules_added": exchanges.len(), "saved": saved["saved"], "problems": saved["problems"]}),
    )
}

pub async fn stop(a: &Value, ctx: &Ctx) -> Result<Value, String> {
    let name = name_of(a)?;
    let set = rules_without(ctx, &rule_prefix(&name)).await?;
    rpc(&ctx.socket, "rules.set", json!({"yaml": set.to_yaml()})).await
}

pub async fn delete(a: &Value, ctx: &Ctx) -> Result<Value, String> {
    let name = name_of(a)?;
    stop(a, ctx).await.ok();
    std::fs::remove_dir_all(root(ctx).join(&name)).map_err(|_| format!("no scenario `{name}`"))?;
    Ok(json!({"deleted": name}))
}

/// Resends a captured call, optionally changed.
pub async fn repeat(a: &Value, ctx: &Ctx) -> Result<Value, String> {
    let key = req(a, "key")?.clone();
    let got = rpc(&ctx.socket, "traffic.get", json!({"key": key})).await?;
    let t: Transaction =
        serde_json::from_value(got["transaction"].clone()).map_err(|e| e.to_string())?;
    let patch = a.get("patch").cloned().unwrap_or_else(|| json!({}));
    let mut headers: Vec<(String, String)> = t
        .request_headers
        .iter()
        .filter(|h| {
            ![
                "host",
                "content-length",
                "connection",
                "proxy-connection",
                "transfer-encoding",
            ]
            .contains(&h.name.to_lowercase().as_str())
        })
        .map(|h| (h.name.clone(), h.value.clone()))
        .collect();
    if let Some(rm) = patch.get("remove_headers").and_then(Value::as_array) {
        for n in rm.iter().filter_map(Value::as_str) {
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case(n));
        }
    }
    if let Some(set) = patch.get("set_headers").and_then(Value::as_object) {
        for (n, v) in set {
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case(n));
            headers.push((n.clone(), v.as_str().unwrap_or("").to_string()));
        }
    }
    let params = json!({
        "method": patch.get("method").and_then(Value::as_str).map(str::to_string).unwrap_or(t.method.clone()),
        "url": patch.get("url").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| t.url()),
        "headers": headers.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>(),
        "body": patch.get("body").and_then(Value::as_str).map(str::to_string).or(t.request_body_preview.clone()).unwrap_or_default(),
        "count": a.get("count").cloned().unwrap_or(json!(1)),
        "parallel": a.get("parallel").cloned().unwrap_or(json!(false)),
    });
    let mut r = rpc(&ctx.socket, "compose.send", params).await?;
    slim(&mut r, true, flag(a, "include_secrets"));
    Ok(r)
}
