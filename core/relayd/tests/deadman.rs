//! The dead-man switch, with real processes: relayd changes the (fake) system proxy, gets killed,
//! and the guard process puts the original settings back. Never touches the machine's network.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use relay_control::Client;
use relay_control::sysproxy::{Endpoint, FileBackend, ProxySettings, SystemProxyBackend};
use serde_json::json;

fn temp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let d = std::env::temp_dir().join(format!("rd-{name}-{nanos}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn original() -> ProxySettings {
    ProxySettings {
        web: Endpoint::default(),
        secure: Endpoint {
            enabled: true,
            host: "corp-proxy.example".into(),
            port: 3128,
        },
        bypass: vec!["*.local".into()],
    }
}

fn start_relayd(dir: &Path) -> (Child, PathBuf, FileBackend) {
    let fake = dir.join("fake-system.json");
    let backend = FileBackend { path: fake.clone() };
    backend.write("x", &original()).unwrap();
    let socket = dir.join("d.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_relayd"))
        .args([
            "--data-dir",
            dir.to_str().unwrap(),
            "--port",
            "0",
            "--socket",
            socket.to_str().unwrap(),
        ])
        .args(["--fake-system-proxy", fake.to_str().unwrap()])
        .env("RELAY_LOG", "warn")
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !socket.exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "relayd did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (child, socket, backend)
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) -> Duration {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "timed out waiting for: {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    t0.elapsed()
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_9_is_recovered_by_the_guard() {
    let dir = temp("kill9");
    let (mut child, socket, backend) = start_relayd(&dir);
    let mut c = Client::connect(&socket).await.unwrap();
    c.call("system_proxy.enable", json!({"port": 18888}))
        .await
        .unwrap();
    assert_eq!(
        backend.read("x").unwrap().web.port,
        18888,
        "relayd pointed the system at itself"
    );

    // The crash Charles users fear: the process dies without any chance to clean up.
    Command::new("kill")
        .args(["-9", &child.id().to_string()])
        .status()
        .unwrap();
    let _ = child.wait();
    let took = wait_until("the guard restores the original settings", || {
        backend.read("x").unwrap() == original()
    });
    eprintln!("restored {took:?} after kill -9");
    assert!(!dir.join("system-proxy.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_restores_immediately_and_removes_the_socket() {
    let dir = temp("term");
    let (mut child, socket, backend) = start_relayd(&dir);
    let mut c = Client::connect(&socket).await.unwrap();
    c.call("system_proxy.enable", json!({"port": 18889}))
        .await
        .unwrap();
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "clean exit");
    assert_eq!(
        backend.read("x").unwrap(),
        original(),
        "restored before exiting"
    );
    assert!(!socket.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_after_a_crash_cleans_up_even_without_the_guard() {
    let dir = temp("restart");
    // Leave behind exactly what a crashed relayd would: changed settings + its record of the original.
    let fake = dir.join("fake-system.json");
    let backend = FileBackend { path: fake.clone() };
    let ours = ProxySettings {
        web: Endpoint {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 18890,
        },
        secure: Endpoint {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 18890,
        },
        bypass: vec![],
    };
    backend.write("x", &ours).unwrap();
    let record =
        json!({"service": "Test Wi-Fi", "original": original(), "ours": ours, "pid": 999_999});
    std::fs::write(dir.join("system-proxy.json"), record.to_string()).unwrap();

    let socket = dir.join("d.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_relayd"))
        .args([
            "--data-dir",
            dir.to_str().unwrap(),
            "--no-proxy",
            "--socket",
            socket.to_str().unwrap(),
        ])
        .args(["--fake-system-proxy", fake.to_str().unwrap()])
        .env("RELAY_LOG", "warn")
        .spawn()
        .unwrap();
    wait_until("relayd starts", || socket.exists());
    assert_eq!(
        backend.read("x").unwrap(),
        original(),
        "startup recovered the network first"
    );
    let _ = child.kill();
    let _ = child.wait();
}
