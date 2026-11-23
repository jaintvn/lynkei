//! The control API end to end: real engine, real socket server, real client, local origin.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use relay_control::sysproxy::{FileBackend, SystemProxyBackend};
use relay_control::{Client, Engine, EngineOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn temp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let d = std::env::temp_dir().join(format!("rc-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A tiny HTTP/1.1 origin: answers every request with `hello from <path>`.
async fn origin() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                let line = String::from_utf8_lossy(&got)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = format!("hello from {path}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

async fn setup(name: &str) -> (Arc<Engine>, Client, u16, PathBuf, Arc<FileBackend>) {
    let dir = temp(name);
    let backend = Arc::new(FileBackend {
        path: dir.join("fake-system.json"),
    });
    let mut opts = EngineOptions::new(&dir);
    opts.system_proxy = Some(backend.clone());
    let engine = Engine::new(opts).unwrap();
    let port = engine
        .start_proxy("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let socket = dir.join("c.sock");
    let listener = relay_control::server::bind(&socket).unwrap();
    tokio::spawn(relay_control::server::serve(listener, engine.clone()));
    let client = Client::connect(&socket).await.unwrap();
    (engine, client, port, dir, backend)
}

async fn get(proxy: u16, url: &str) {
    let r = relay_proxy::client::fetch_via_proxy(
        format!("127.0.0.1:{proxy}").parse().unwrap(),
        url,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn hello_search_get_and_clear() {
    let (_engine, mut c, proxy, _dir, _) = setup("search").await;
    let hello = c.call("hello", Value::Null).await.unwrap();
    assert_eq!(hello["api"], "relay.control/1");
    assert_eq!(hello["proxy"]["port"], proxy);

    let o = origin().await;
    get(proxy, &format!("http://127.0.0.1:{o}/v2/products?page=2")).await;
    get(proxy, &format!("http://127.0.0.1:{o}/v2/checkout")).await;

    let rows = c
        .call("traffic.search", json!({"query": "path:/v2/checkout"}))
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(
        rows[0]["transaction"]["response_body_preview"],
        "hello from /v2/checkout"
    );
    let all = c
        .call("traffic.search", json!({"query": "", "limit": 10}))
        .await
        .unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(
        all[0]["transaction"]["path"], "/v2/checkout",
        "newest first"
    );

    let key = rows[0]["key"].as_i64().unwrap();
    let one = c.call("traffic.get", json!({"key": key})).await.unwrap();
    assert_eq!(one["transaction"]["method"], "GET");

    c.call("traffic.clear", Value::Null).await.unwrap();
    assert!(
        c.call("traffic.search", json!({}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn subscribers_get_live_events_matching_their_filter() {
    let (_engine, mut c, proxy, _dir, _) = setup("subscribe").await;
    c.call("traffic.subscribe", json!({"query": "path:/wanted"}))
        .await
        .unwrap();
    let o = origin().await;
    get(proxy, &format!("http://127.0.0.1:{o}/ignored")).await;
    get(proxy, &format!("http://127.0.0.1:{o}/wanted")).await;
    let t = tokio::time::timeout(Duration::from_secs(3), c.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(t.path, "/wanted", "non-matching traffic is not pushed");
}

#[tokio::test]
async fn rules_are_validated_before_they_are_saved() {
    let (_engine, mut c, _proxy, dir, _) = setup("rules").await;
    let bad = c
        .call(
            "rules.set",
            json!({"yaml": "version: 1\nrules:\n  - id: x\n    kind: block\n    match: {}\n"}),
        )
        .await
        .unwrap();
    assert_eq!(bad["saved"], false);
    assert!(
        bad["problems"][0]
            .as_str()
            .unwrap()
            .contains("`match` is empty")
    );
    assert!(
        !dir.join("rules.yaml").exists(),
        "invalid rules are never written"
    );

    let good = "version: 1\nrules:\n  - id: no-ads\n    kind: block\n    match: { host: \"*.doubleclick.net\" }\n";
    let ok = c.call("rules.set", json!({"yaml": good})).await.unwrap();
    assert_eq!(ok["saved"], true);
    assert_eq!(
        std::fs::read_to_string(dir.join("rules.yaml")).unwrap(),
        good
    );
    let back = c.call("rules.get", Value::Null).await.unwrap();
    assert!(back["yaml"].as_str().unwrap().contains("doubleclick"));
}

#[tokio::test]
async fn system_proxy_through_the_api_and_clear_errors() {
    let (_engine, mut c, proxy, _dir, backend) = setup("sysproxy").await;
    let on = c.call("system_proxy.enable", Value::Null).await.unwrap();
    assert_eq!(
        (on["enabled"].as_bool(), on["port"].as_u64()),
        (Some(true), Some(proxy as u64))
    );
    assert_eq!(backend.read("x").unwrap().secure.port, proxy);
    let off = c.call("system_proxy.disable", Value::Null).await.unwrap();
    assert_eq!(off["enabled"], false);
    assert!(!backend.read("x").unwrap().secure.enabled);

    let e = c
        .call("nope.nothing", Value::Null)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown method") && e.contains("-32601"), "{e}");
    let e = c
        .call("traffic.search", json!({"query": "stauts:500"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown filter") && e.contains("-32602"), "{e}");
}

#[tokio::test]
async fn only_one_daemon_per_socket() {
    let (_engine, _c, _proxy, dir, _) = setup("single").await;
    let err = relay_control::server::bind(&dir.join("c.sock"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("already running"), "{err}");
}

struct ScriptedRunner;

impl relay_devices::Runner for ScriptedRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<relay_devices::Output> {
        let line = format!("{program} {}", args.join(" "));
        let out = |ok: bool, s: &str| relay_devices::Output {
            ok,
            stdout: s.into(),
            stderr: String::new(),
        };
        Ok(if line.contains("simctl list") {
            out(
                true,
                r#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-26-4":[{"udid":"6507CF9D-A8F8-4285-96CF-53780AE0EB92","name":"iPhone 17 Pro","state":"Booted"}]}}"#,
            )
        } else if line.contains("LocalHostName") {
            out(true, "test-mac\n")
        } else {
            out(true, "")
        })
    }
}

#[tokio::test]
async fn devices_are_listed_connected_and_disconnected_through_the_api() {
    let dir = temp("devices");
    let backend = Arc::new(FileBackend {
        path: dir.join("fake-system.json"),
    });
    let mut opts = EngineOptions::new(&dir);
    opts.system_proxy = Some(backend.clone());
    opts.devices_runner = Some(Arc::new(ScriptedRunner));
    let engine = Engine::new(opts).unwrap();
    let port = engine
        .start_proxy("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let socket = dir.join("c.sock");
    tokio::spawn(relay_control::server::serve(
        relay_control::server::bind(&socket).unwrap(),
        engine.clone(),
    ));
    let mut c = Client::connect(&socket).await.unwrap();

    let list = c.call("devices.list", Value::Null).await.unwrap();
    assert_eq!(list["devices"][0]["name"], "iPhone 17 Pro");
    assert_eq!(
        list["wifi_setup_url"],
        format!("http://test-mac.local:{port}/setup")
    );

    let id = "6507CF9D-A8F8-4285-96CF-53780AE0EB92";
    let r = c.call("devices.connect", json!({"id": id})).await.unwrap();
    assert_eq!(r["ready"], true, "{r}");
    assert_eq!(r["checklist"]["certificate"]["state"], "done");
    assert_eq!(
        backend.read("x").unwrap().secure.port,
        port,
        "system proxy now points at Relay"
    );
    let again = c.call("devices.list", Value::Null).await.unwrap();
    assert_eq!(again["devices"][0]["connected"], true);

    c.call("devices.disconnect", json!({"id": id}))
        .await
        .unwrap();
    assert!(
        !backend.read("x").unwrap().secure.enabled,
        "last simulator disconnected → proxy off"
    );
    let e = c
        .call("devices.connect", json!({"id": "nope"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("not running or attached"), "{e}");
}

#[tokio::test]
async fn breakpoints_are_listed_edited_resumed_and_aborted_through_the_api() {
    let (_engine, mut c, proxy, _dir, _) = setup("breakpoints").await;
    let origin = origin().await;
    let rules = "version: 1\nrules:\n  - { id: hold, kind: breakpoint, match: { path: /held }, phase: request }\n  - { id: hold-resp, kind: breakpoint, match: { path: /answer }, phase: response }\n";
    assert_eq!(
        c.call("rules.set", json!({"yaml": rules})).await.unwrap()["saved"],
        true
    );
    assert_eq!(
        c.call("breakpoints.list", Value::Null).await.unwrap(),
        json!([])
    );

    // A held request: wait for it, add a header, let it go.
    let url = format!("http://127.0.0.1:{origin}/held?x=1");
    let client = tokio::spawn({
        let url = url.clone();
        async move { get(proxy, &url).await }
    });
    let held = c
        .call("breakpoints.wait", json!({"timeout_ms": 5000}))
        .await
        .unwrap();
    assert_eq!(held[0]["phase"], "request");
    assert_eq!(held[0]["rule"], "hold");
    assert_eq!(held[0]["method"], "GET");
    let id = held[0]["tx_id"].as_u64().unwrap();
    assert!(!client.is_finished(), "the app is waiting");
    let done = c
        .call(
            "breakpoints.resume",
            json!({"tx_id": id, "edit": {"set_headers": {"x-edited": "by-api"}}}),
        )
        .await
        .unwrap();
    assert_eq!(done["done"], true);
    client.await.unwrap();
    let seen = c
        .call("traffic.search", json!({"query": "header:x-edited=by-api"}))
        .await
        .unwrap();
    assert_eq!(
        seen.as_array().unwrap().len(),
        1,
        "the edit reached the server"
    );

    // A held response: change its status and body.
    let url = format!("http://127.0.0.1:{origin}/answer");
    let client = tokio::spawn(async move {
        relay_proxy::client::fetch_via_proxy(
            format!("127.0.0.1:{proxy}").parse().unwrap(),
            &url,
            &[],
        )
        .await
        .unwrap()
    });
    let held = c
        .call("breakpoints.wait", json!({"timeout_ms": 5000}))
        .await
        .unwrap();
    assert_eq!(held[0]["phase"], "response");
    assert_eq!(held[0]["status"], 200);
    assert_eq!(held[0]["body"], "hello from /answer");
    let id = held[0]["tx_id"].as_u64().unwrap();
    c.call(
        "breakpoints.resume",
        json!({"tx_id": id, "phase": "response", "edit": {"status": 503, "body": "down for maintenance"}}),
    )
    .await
    .unwrap();
    let res = client.await.unwrap();
    assert_eq!(res.status, 503);
    assert_eq!(&res.body[..], b"down for maintenance");

    // Abort, and the error cases.
    let url = format!("http://127.0.0.1:{origin}/held?abort=1");
    let client = tokio::spawn(async move {
        relay_proxy::client::fetch_via_proxy(
            format!("127.0.0.1:{proxy}").parse().unwrap(),
            &url,
            &[],
        )
        .await
    });
    let held = c
        .call("breakpoints.wait", json!({"timeout_ms": 5000}))
        .await
        .unwrap();
    let id = held[0]["tx_id"].as_u64().unwrap();
    assert_eq!(
        c.call("breakpoints.abort", json!({"tx_id": id}))
            .await
            .unwrap()["done"],
        true
    );
    let aborted = client.await.unwrap();
    assert!(
        aborted.map_or(true, |r| r.status >= 500),
        "the app got an error"
    );
    assert!(
        c.call("breakpoints.resume", json!({"tx_id": id}))
            .await
            .is_err(),
        "already released"
    );
    assert!(
        c.call("breakpoints.resume", json!({"tx_id": 1, "phase": "later"}))
            .await
            .is_err()
    );
    // Nothing held: wait times out with an empty list.
    let none = c
        .call("breakpoints.wait", json!({"timeout_ms": 150}))
        .await
        .unwrap();
    assert_eq!(none, json!([]));
}

#[tokio::test]
async fn decrypt_and_throttle_switches_work_through_the_api() {
    let (engine, mut c, proxy, _dir, _) = setup("switches").await;
    let origin = origin().await;
    let url = format!("http://127.0.0.1:{origin}/x");

    let s = c.call("settings.get", Value::Null).await.unwrap();
    assert_eq!(s["decrypt"], true);
    assert_eq!(s["throttle"], Value::Null);
    assert!(
        s["throttle_presets"]
            .as_array()
            .unwrap()
            .contains(&json!("lte"))
    );

    // A bad value changes nothing at all.
    assert!(
        c.call(
            "settings.set",
            json!({"decrypt": false, "throttle": "warp"})
        )
        .await
        .is_err()
    );
    assert_eq!(
        c.call("settings.get", Value::Null).await.unwrap()["decrypt"],
        true
    );
    assert!(
        c.call("settings.set", json!({"decrypt": "yes"}))
            .await
            .is_err()
    );

    let t0 = std::time::Instant::now();
    get(proxy, &url).await;
    let baseline = t0.elapsed();

    let s = c
        .call(
            "settings.set",
            json!({"decrypt": false, "throttle": "high-latency"}),
        )
        .await
        .unwrap();
    assert_eq!(
        (s["decrypt"].clone(), s["throttle"].clone()),
        (json!(false), json!("high-latency"))
    );
    let t0 = std::time::Instant::now();
    get(proxy, &url).await;
    assert!(t0.elapsed() >= baseline + Duration::from_millis(600));

    // Survives a proxy restart.
    c.call("proxy.stop", Value::Null).await.unwrap();
    let port = engine
        .start_proxy("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let t0 = std::time::Instant::now();
    get(port, &url).await;
    assert!(
        t0.elapsed() >= Duration::from_millis(700),
        "the throttle is still on"
    );

    // Partial updates leave the other switch alone; null turns the throttle off.
    let s = c
        .call("settings.set", json!({"throttle": null}))
        .await
        .unwrap();
    assert_eq!(
        (s["decrypt"].clone(), s["throttle"].clone()),
        (json!(false), Value::Null)
    );
    let t0 = std::time::Instant::now();
    get(port, &url).await;
    assert!(t0.elapsed() < Duration::from_millis(600));
}
