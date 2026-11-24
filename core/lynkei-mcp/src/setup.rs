//! Zero-setup for AI agents: start `lynkeid` on demand, and register this server with agent apps.
//!
//! * `ensure_daemon` is what makes `lynkei-mcp` work the moment an agent launches it. If no daemon
//!   answers on the control socket, it starts `lynkeid` (found next to this binary, in the app
//!   bundle, via `LYNKEI_DAEMON` or on `PATH`) and waits until it is ready.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::json;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn remember_data_dir(dir: &Path) {
    let _ = DATA_DIR.set(dir.to_path_buf());
}

/// Makes sure something answers on `socket`; starts `lynkeid` if nothing does.
pub async fn ensure_daemon(socket: &Path) -> Result<(), String> {
    if lynkei_control::Client::connect(socket).await.is_ok() {
        return Ok(());
    }
    let data_dir = DATA_DIR
        .get()
        .cloned()
        .or_else(|| socket.parent().map(Path::to_path_buf))
        .ok_or("no data directory")?;
    let daemon = find_daemon().ok_or_else(|| {
        "Lynkei's engine (lynkeid) is not running and could not be found to start it. Open the \
         Lynkei app once, or set LYNKEI_DAEMON to the path of lynkeid."
            .to_string()
    })?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("creating {}: {e}", data_dir.display()))?;
    let port = free_proxy_port();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("lynkeid.log"))
        .map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&daemon);
    cmd.arg("--data-dir")
        .arg(&data_dir)
        .arg("--socket")
        .arg(socket)
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // Its own process group, so it outlives this server and the agent that started it.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn()
        .map_err(|e| format!("starting {}: {e}", daemon.display()))?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if lynkei_control::Client::connect(socket).await.is_ok() {
            return Ok(());
        }
    }
    Err(format!(
        "started lynkeid but it did not become ready; see {}",
        data_dir.join("lynkeid.log").display()
    ))
}

/// 8888 when it is free, else the first free port above it (the Mac app may be using 8888).
fn free_proxy_port() -> u16 {
    (8888..8900)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap_or(8888)
}

fn find_daemon() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LYNKEI_DAEMON") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe().and_then(|e| e.canonicalize())
        && let Some(dir) = exe.parent()
    {
        for rel in ["lynkeid", "../Helpers/lynkeid", "../MacOS/lynkeid"] {
            let p = dir.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let p = d.join("lynkeid");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let app = PathBuf::from("/Applications/Lynkei.app/Contents/Helpers/lynkeid");
    app.is_file().then_some(app)
}

fn own_path() -> String {
    std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "lynkei-mcp".into())
}

pub const HELP: &str = "lynkei-mcp: Lynkei's Model Context Protocol server.

Usage:
  lynkei-mcp                     run over stdio (what an agent launches)
  lynkei-mcp --http [ADDR]       serve MCP over HTTP at http://ADDR/mcp (default 127.0.0.1:8890)
  lynkei-mcp doctor              check that the engine can be reached

It talks to the Lynkei app's engine (or starts its own `lynkeid` when the app is closed).
Options: --socket <path>, --data-dir <dir> (default: Lynkei's data folder; or LYNKEI_DATA_DIR)";

pub async fn doctor(socket: &Path) -> i32 {
    println!("lynkei-mcp {}", env!("CARGO_PKG_VERSION"));
    println!("binary:  {}", own_path());
    println!("socket:  {}", socket.display());
    match find_daemon() {
        Some(d) => println!("engine:  {}", d.display()),
        None => println!("engine:  NOT FOUND (open the Lynkei app once, or set LYNKEI_DAEMON)"),
    }
    match ensure_daemon(socket).await {
        Ok(()) => match crate::rpc(socket, "hello", json!({})).await {
            Ok(s) => {
                println!(
                    "status:  ready\n{}",
                    serde_json::to_string_pretty(&s).unwrap_or_default()
                );
                0
            }
            Err(e) => {
                println!("status:  engine answered but status failed: {e}");
                1
            }
        },
        Err(e) => {
            println!("status:  {e}");
            1
        }
    }
}
