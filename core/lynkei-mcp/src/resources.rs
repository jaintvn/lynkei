//! Read-only views of Lynkei (`lynkei://…`) that an agent can read, and subscribe to for updates.

use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, rpc, slim};

const URIS: [&str; 5] = [
    "lynkei://status",
    "lynkei://rules",
    "lynkei://devices",
    "lynkei://paused",
    "lynkei://traffic",
];

pub fn list() -> Vec<Value> {
    vec![
        json!({"uri": "lynkei://status", "name": "Status", "description": "Proxy, store and certificate state", "mimeType": "application/json"}),
        json!({"uri": "lynkei://rules", "name": "Rules", "description": "The rules as YAML", "mimeType": "application/json"}),
        json!({"uri": "lynkei://devices", "name": "Devices", "description": "Simulators, emulators and phones", "mimeType": "application/json"}),
        json!({"uri": "lynkei://paused", "name": "Paused calls", "description": "Requests and responses held at a pause rule. Subscribe to hear when one is held.", "mimeType": "application/json"}),
        json!({"uri": "lynkei://traffic", "name": "Latest traffic", "description": "The 20 newest calls. Subscribe to hear about new ones.", "mimeType": "application/json"}),
    ]
}

pub fn templates() -> Vec<Value> {
    vec![
        json!({"uriTemplate": "lynkei://traffic{?query}", "name": "Traffic matching a search",
        "description": "The 20 newest calls matching a query such as status:5xx host:api.acme.com. Subscribe to hear about new matches.", "mimeType": "application/json"}),
    ]
}

fn base(uri: &str) -> &str {
    uri.split('?').next().unwrap_or(uri)
}

pub fn known(uri: &str) -> bool {
    URIS.contains(&base(uri))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_of(uri: &str) -> String {
    uri.split_once('?')
        .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("query=")))
        .map(percent_decode)
        .unwrap_or_default()
}

fn call_for(uri: &str) -> (&'static str, Value) {
    match base(uri) {
        "lynkei://rules" => ("rules.get", Value::Null),
        "lynkei://devices" => ("devices.list", Value::Null),
        "lynkei://paused" => ("breakpoints.list", Value::Null),
        "lynkei://traffic" => (
            "traffic.search",
            json!({"query": query_of(uri), "limit": 20}),
        ),
        _ => ("hello", Value::Null),
    }
}

pub async fn read(uri: &str, socket: &std::path::Path) -> Result<String, String> {
    if !known(uri) {
        return Err(format!("unknown resource {uri}"));
    }
    let (method, params) = call_for(uri);
    let mut v = rpc(socket, method, params).await?;
    if base(uri) == "lynkei://traffic" {
        slim(&mut v, false, false);
    }
    Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
}

/// Starts watching `uri`; every change sends `notifications/resources/updated` to the client.
pub fn subscribe(ctx: &Ctx, uri: &str) {
    let out = ctx.out.clone();
    let socket = ctx.socket.clone();
    let u = uri.to_string();
    let notify = move |u: &str| {
        let _ = out.send(json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": u}}));
    };
    let handle = tokio::spawn(async move {
        if base(&u) == "lynkei://traffic" {
            // Live: one connection, one notification per matching new call.
            loop {
                if let Ok(mut c) = lynkei_control::Client::connect(&socket).await
                    && c.call("traffic.subscribe", json!({"query": query_of(&u)}))
                        .await
                        .is_ok()
                {
                    while let Ok(Some(_)) = c.next_event().await {
                        notify(&u);
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        } else {
            // Everything else: look again and tell the client when the answer changes.
            let every = if base(&u) == "lynkei://paused" {
                700
            } else {
                2000
            };
            let (method, params) = call_for(&u);
            let mut last: Option<String> = None;
            loop {
                if let Ok(v) = rpc(&socket, method, params.clone()).await {
                    let text = v.to_string();
                    if last.as_ref().is_some_and(|l| *l != text) {
                        notify(&u);
                    }
                    last = Some(text);
                }
                tokio::time::sleep(Duration::from_millis(every)).await;
            }
        }
    });
    if let Some(old) = ctx.subs.lock().unwrap().insert(uri.to_string(), handle) {
        old.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_decoded_from_the_uri() {
        assert_eq!(
            query_of("lynkei://traffic?query=status%3A5xx+host%3Aapi"),
            "status:5xx host:api"
        );
        assert_eq!(query_of("lynkei://traffic"), "");
        assert!(known("lynkei://traffic?query=x") && !known("lynkei://nope"));
    }
}
