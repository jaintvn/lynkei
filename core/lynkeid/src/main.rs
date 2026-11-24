//! lynkeid — the Lynkei engine daemon (proxy + store + control API + system-proxy guard).
//!
//!   lynkeid [--data-dir DIR] [--port 8888] [--listen 127.0.0.1] [--socket PATH] [--no-proxy]
//!          [--fake-system-proxy FILE]
//!   lynkeid guard --pid PID --data-dir DIR [--fake-system-proxy FILE]      (internal)
//!   lynkeid launch-agent [--write]   print (or write) the launchd agent that keeps lynkeid running
//!
//! `--fake-system-proxy FILE` keeps "system" proxy settings in a JSON file instead of changing the
//! Mac's network — used by tests and CI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use lynkei_control::sysproxy::{self, FileBackend, Networksetup, SystemProxyBackend};
use lynkei_control::{Engine, EngineOptions};

const LAUNCH_AGENT_LABEL: &str = "dev.lynkei.lynkeid";

fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LYNKEI_DATA_DIR") {
        return d.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Lynkei")
    } else {
        PathBuf::from(home).join(".lynkei")
    }
}

struct Args {
    cmd: Option<String>,
    opts: HashMap<String, String>,
    flags: Vec<String>,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut cmd = None;
    let mut opts = HashMap::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if let Some(k) = raw[i].strip_prefix("--") {
            let is_flag = matches!(k, "no-proxy" | "write");
            if !is_flag && i + 1 < raw.len() {
                opts.insert(k.to_string(), raw[i + 1].clone());
                i += 1;
            } else {
                flags.push(k.to_string());
            }
        } else if cmd.is_none() {
            cmd = Some(raw[i].clone());
        }
        i += 1;
    }
    Args { cmd, opts, flags }
}

fn backend(args: &Args) -> Option<Arc<dyn SystemProxyBackend>> {
    match args.opts.get("fake-system-proxy") {
        Some(path) => Some(Arc::new(FileBackend { path: path.into() })),
        None if cfg!(target_os = "macos") => Some(Arc::new(Networksetup)),
        None => None,
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LYNKEI_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = parse_args();
    let data_dir = args
        .opts
        .get("data-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_data_dir);
    match args.cmd.as_deref() {
        None => run_daemon(&args, data_dir),
        Some("guard") => run_guard(&args, &data_dir),
        Some("launch-agent") => launch_agent(&args, &data_dir),
        Some(other) => bail!(
            "unknown command `{other}` (expected: guard, launch-agent, or no command to run the daemon)"
        ),
    }
}

fn run_guard(args: &Args, data_dir: &Path) -> Result<()> {
    let pid: u32 = args.opts.get("pid").context("--pid is required")?.parse()?;
    let backend = backend(args).context("no system proxy backend on this platform")?;
    let state = data_dir.join(sysproxy::STATE_FILE);
    if sysproxy::guard(backend.as_ref(), &state, pid, Duration::from_millis(500))? {
        tracing::warn!(
            pid,
            "lynkeid exited without restoring the system proxy; restored it"
        );
    }
    Ok(())
}

/// Starts the guard in its own process group, so Ctrl-C or `kill -9` of lynkeid leaves it running.
fn spawn_guard(args: &Args, data_dir: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("guard")
        .arg("--pid")
        .arg(std::process::id().to_string())
        .arg("--data-dir")
        .arg(data_dir);
    if let Some(f) = args.opts.get("fake-system-proxy") {
        cmd.arg("--fake-system-proxy").arg(f);
    }
    cmd.process_group(0).stdin(std::process::Stdio::null());
    cmd.spawn().context("starting the system proxy guard")?;
    Ok(())
}

fn run_daemon(args: &Args, data_dir: PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("lynkeid")
        .build()?;
    runtime.block_on(async {
        let mut opts = EngineOptions::new(&data_dir);
        opts.system_proxy = backend(args);
        // Engine::new also recovers system proxy settings left behind by a crash.
        let engine = Engine::new(opts)?;
        if engine.system_proxy().is_some() {
            spawn_guard(args, &data_dir)?;
        }
        if !args.flags.iter().any(|f| f == "no-proxy") {
            let port: u16 = args.opts.get("port").map(|p| p.parse()).transpose()?.unwrap_or(8888);
            let ip = args.opts.get("listen").map(String::as_str).unwrap_or("127.0.0.1");
            let port = engine.start_proxy(format!("{ip}:{port}").parse()?).await?;
            tracing::info!(port, "proxy listening");
        }
        let socket = args.opts.get("socket").map(PathBuf::from).unwrap_or_else(|| lynkei_control::default_socket(&data_dir));
        let listener = lynkei_control::server::bind(&socket)?;
        tracing::info!(socket = %socket.display(), api = lynkei_control::API_VERSION, "control API ready");
        let server = tokio::spawn(lynkei_control::server::serve(listener, engine.clone()));

        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = server => {}
        }
        tracing::info!("shutting down: restoring system proxy, stopping proxy");
        engine.shutdown().await;
        let _ = std::fs::remove_file(&socket);
        Ok(())
    })
}

fn launch_agent(args: &Args, data_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = data_dir.join("lynkeid.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>--data-dir</string><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        exe.display(),
        data_dir.display(),
        log.display()
    );
    if args.flags.iter().any(|f| f == "write") {
        let home = std::env::var("HOME")?;
        let path =
            PathBuf::from(home).join(format!("Library/LaunchAgents/{LAUNCH_AGENT_LABEL}.plist"));
        std::fs::write(&path, plist)?;
        println!("wrote {}", path.display());
        println!(
            "load it with: launchctl bootstrap gui/$(id -u) {}",
            path.display()
        );
    } else {
        print!("{plist}");
    }
    Ok(())
}
