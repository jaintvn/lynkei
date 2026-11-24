//! Ready-made workflows an agent's UI can offer. Each is a short instruction naming the tools to use.

use serde_json::{Value, json};

pub fn list() -> Vec<Value> {
    vec![
        json!({"name": "debug_failing_endpoint", "description": "Find out why an endpoint is failing and what changed.",
            "arguments": [{"name": "endpoint", "description": "path or host to look at (optional)", "required": false}]}),
        json!({"name": "mock_api_offline", "description": "Record an API's responses and replay them as mocks so the app works offline.",
            "arguments": [{"name": "endpoint", "description": "path or host to mock", "required": true}]}),
        json!({"name": "find_slow_calls", "description": "List the slowest calls and say what makes them slow.",
            "arguments": [{"name": "threshold_ms", "description": "slower than this, default 800", "required": false}]}),
    ]
}

pub fn get(name: &str, args: &Value) -> Option<Value> {
    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let (description, text) = match name {
        "debug_failing_endpoint" => {
            let target = arg("endpoint").map(|e| format!(" {e}")).unwrap_or_default();
            (
                "Debug a failing endpoint",
                format!(
                    "Debug the failing calls{target} in Lynkei.\n1. `status` to check the proxy is running; if not, `proxy_start`.\n2. `search_traffic` with `status:5xx{t} ` and again with `status:4xx{t}` (limit 20) to see what is failing and how often.\n3. `get_transaction` on the newest failure with include_bodies true; read the error body and the request that caused it.\n4. Compare with a recent success of the same endpoint (`search_traffic` with `status:2xx`). Say what differs: headers, body, app version, device.\n5. If a rule from `list_rules` touched the call (see applied_rules), say so.\nFinish with the likely cause and one thing to try. Do not change rules unless asked.",
                    t = arg("endpoint")
                        .map(|e| format!(" path:{e}"))
                        .unwrap_or_default()
                ),
            )
        }
        "mock_api_offline" => {
            let e = arg("endpoint")?;
            (
                "Mock an API for offline use",
                format!(
                    "Make `{e}` work offline using Lynkei.\n1. Make sure the app has made the calls you need: `search_traffic` with `path:{e}` (or `host:{e}`).\n2. `record_scenario` with a short name and that query.\n3. `replay_scenario` with the name.\n4. Check with `list_rules` that the mock rules are on, then ask the user to repeat the action in the app, or use `send_request` to try one call, and confirm with `search_traffic` that the call was answered by a rule.\nTell the user how to stop it (`stop_scenario`)."
                ),
            )
        }
        "find_slow_calls" => {
            let ms = arg("threshold_ms").unwrap_or("800");
            (
                "Find slow calls",
                format!(
                    "Find slow calls in Lynkei.\n1. `search_traffic` with `duration:>{ms}ms` (limit 50).\n2. Group them by endpoint and report count and worst time for each.\n3. For the two slowest endpoints, `get_transaction` on one call and look at size, protocol and whether a throttle rule or `get_settings` throttle is on.\nSay which are slow because of the server, the payload size or a throttle."
                ),
            )
        }
        _ => return None,
    };
    Some(
        json!({"description": description, "messages": [{"role": "user", "content": {"type": "text", "text": text}}]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_listed_and_filled_in() {
        assert_eq!(list().len(), 3);
        let p = get(
            "debug_failing_endpoint",
            &json!({"endpoint": "/v2/checkout"}),
        )
        .unwrap();
        assert!(
            p["messages"][0]["content"]["text"]
                .as_str()
                .unwrap()
                .contains("path:/v2/checkout")
        );
        assert!(
            get("mock_api_offline", &json!({})).is_none(),
            "endpoint is required"
        );
        assert!(get("nope", &json!({})).is_none());
    }
}
