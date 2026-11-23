//! relay — command-line front end for the Relay engine.
//!
//! Talking to a running relayd (control API, `--socket PATH` to override):
//!   relay status
//!   relay search [filter…] [--limit 50] [--json]     e.g. relay search status:5xx host:api.acme.com
//!   relay tail [filter…]                              live events
//!   relay rules check <file> | push <file> | pull
//!   relay proxy start [--port 8888] | stop
//!   relay system-proxy on | off | status
//!   relay devices [list] | connect <id> | disconnect <id>
//!
//! Standalone:
//!   relay serve [--port 8888] [--listen 0.0.0.0] [--data-dir DIR] [--json]
//!   relay fetch <url> [--proxy 127.0.0.1:8888] [--data-dir DIR]   (trusts only the Relay CA)
//!   relay ca [--data-dir DIR]                                     (prints the CA certificate path)

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use relay_proxy::client::fetch_via_proxy;
use relay_proxy::{CertificateDer, ProxyConfig, Transaction, TransactionState};

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

struct Args {
    cmd: String,
    positional: Vec<String>,
    opts: std::collections::HashMap<String, String>,
    flags: std::collections::HashSet<String>,
}

/// Options (`--name value`, or flags like `--json`) may appear anywhere; the first other word is
/// the command and the rest are positional (filter terms like `-state:tunneled` included).
fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (mut cmd, mut positional, mut opts, mut flags) = (
        None,
        vec![],
        std::collections::HashMap::new(),
        std::collections::HashSet::new(),
    );
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        if let Some(name) = a.strip_prefix("--") {
            if matches!(name, "json" | "write") {
                flags.insert(name.to_string());
            } else if let Some(v) = raw.get(i + 1) {
                opts.insert(name.to_string(), v.clone());
                i += 1;
            }
        } else if cmd.is_none() {
            cmd = Some(a.clone());
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    Args {
        cmd: cmd.unwrap_or_else(|| "help".into()),
        positional,
        opts,
        flags,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RELAY_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = parse_args();
    let data_dir = args
        .opts
        .get("data-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_data_dir);

    match args.cmd.as_str() {
        "serve" => serve(&args, data_dir).await,
        "fetch" => fetch(&args, data_dir).await,
        "get" => get(&args).await,
        "status" | "search" | "tail" | "rules" | "proxy" | "system-proxy" | "devices" => {
            control(&args, &data_dir).await
        }
        "ca" => {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let ca = relay_proxy::ca::CertAuthority::load_or_create(&data_dir, provider)?;
            println!("{}", ca.ca_path().display());
            Ok(())
        }
        _ => {
            eprintln!(
                "usage (with relayd running):\n  relay status\n  relay search [filter…] [--limit 50] [--json]   e.g. relay search status:5xx host:api.acme.com\n  relay tail [filter…]\n  relay rules check <file> | push <file> | pull\n  relay proxy start [--port 8888] | stop\n  relay system-proxy on | off | status\n  relay devices [list] | connect <id> | disconnect <id>\n\nstandalone:\n  relay serve [--port 8888] [--listen 127.0.0.1] [--json]\n  relay get <url> [--ca FILE]\n  relay fetch <url> [--proxy 127.0.0.1:8888]\n  relay ca\n\ncommon: --data-dir DIR, --socket PATH"
            );
            Ok(())
        }
    }
}

async fn serve(args: &Args, data_dir: PathBuf) -> Result<()> {
    let port: u16 = args
        .opts
        .get("port")
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(8888);
    let ip = args
        .opts
        .get("listen")
        .map(String::as_str)
        .unwrap_or("127.0.0.1");
    let listen: SocketAddr = format!("{ip}:{port}").parse()?;
    let json = args.flags.contains("json");
    let cfg = ProxyConfig::new(listen, &data_dir);
    let sink = move |t: Transaction| {
        if json {
            println!("{}", serde_json::to_string(&t).unwrap_or_default());
        } else {
            println!("{}", one_line(&t));
        }
    };
    let handle = relay_proxy::start(cfg, Arc::new(sink)).await?;
    eprintln!("Relay proxy listening on {}", handle.local_addr);
    eprintln!("CA certificate: {}", handle.ca_path().display());
    eprintln!(
        "Devices can also download it from http://{} via the proxy. Ctrl-C to stop.",
        relay_proxy::CA_DOWNLOAD_HOST
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn fetch(args: &Args, data_dir: PathBuf) -> Result<()> {
    let Some(url) = args.positional.first() else {
        bail!("usage: relay fetch <url> [--proxy host:port]")
    };
    let proxy: SocketAddr = args
        .opts
        .get("proxy")
        .map(String::as_str)
        .unwrap_or("127.0.0.1:8888")
        .parse()?;
    let pem = std::fs::read_to_string(data_dir.join(relay_proxy::ca::CA_CERT_FILE))
        .with_context(|| format!("reading Relay CA from {}", data_dir.display()))?;
    use rustls::pki_types::pem::PemObject;
    let ca = CertificateDer::from_pem_slice(pem.as_bytes())?;
    let r = fetch_via_proxy(proxy, url, &[ca]).await?;
    println!(
        "HTTP {} ({} bytes) — TLS verified against the Relay CA only",
        r.status,
        r.body.len()
    );
    for (k, v) in r.headers.iter().take(8) {
        println!("  {k}: {v}");
    }
    Ok(())
}

/// Direct GET (no proxy). `--ca FILE` trusts only that CA (proves interception); default trusts the public web PKI.
async fn get(args: &Args) -> Result<()> {
    let Some(url) = args.positional.first() else {
        bail!("usage: relay get <url> [--ca relay-ca.pem]")
    };
    let ca = match args.opts.get("ca") {
        Some(path) => {
            use rustls::pki_types::pem::PemObject;
            Some(vec![CertificateDer::from_pem_slice(
                std::fs::read_to_string(path)?.as_bytes(),
            )?])
        }
        None => None,
    };
    let started = std::time::Instant::now();
    let r = relay_proxy::client::fetch_direct(url, ca.as_deref()).await?;
    let secs = started.elapsed().as_secs_f64();
    let trust = if ca.is_some() {
        "Relay CA only"
    } else {
        "public web PKI"
    };
    println!(
        "HTTP {} — {} bytes in {:.2}s ({:.1} Mbit/s) — TLS trust: {trust}",
        r.status,
        r.body.len(),
        secs,
        r.body.len() as f64 * 8.0 / secs / 1e6
    );
    Ok(())
}

async fn control(args: &Args, data_dir: &std::path::Path) -> Result<()> {
    use serde_json::{Value, json};
    let socket = args
        .opts
        .get("socket")
        .map(PathBuf::from)
        .unwrap_or_else(|| relay_control::default_socket(data_dir));
    let sub = args.positional.first().map(String::as_str).unwrap_or("");

    // `rules check` works offline.
    if args.cmd == "rules" && sub == "check" {
        let file = args
            .positional
            .get(1)
            .context("usage: relay rules check <file>")?;
        let set = relay_model::rules::RuleSet::from_yaml(&std::fs::read_to_string(file)?)
            .map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
        let problems = set.validate();
        if problems.is_empty() {
            println!("{file}: {} rules, valid", set.rules.len());
            return Ok(());
        }
        for p in &problems {
            println!("{file}: {p}");
        }
        std::process::exit(1);
    }

    let mut c = relay_control::Client::connect(&socket).await?;
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match (args.cmd.as_str(), sub) {
        ("status", _) => {
            let h = c.call("hello", Value::Null).await?;
            if args.flags.contains("json") {
                println!("{}", pretty(&h));
            } else {
                let proxy = if h["proxy"]["running"].as_bool() == Some(true) {
                    format!("listening on {}", h["proxy"]["port"])
                } else {
                    "stopped".into()
                };
                let sys = match h["system_proxy"]["enabled"].as_bool() {
                    Some(true) => format!(
                        "on ({} → {})",
                        h["system_proxy"]["service"].as_str().unwrap_or("?"),
                        h["system_proxy"]["port"]
                    ),
                    Some(false) => "off".into(),
                    None => "n/a".into(),
                };
                println!(
                    "relayd {} · API {}",
                    h["engine_version"].as_str().unwrap_or("?"),
                    h["api"].as_str().unwrap_or("?")
                );
                println!("  proxy         {proxy}");
                println!("  system proxy  {sys}");
                println!(
                    "  stored        {} transactions · {} rules",
                    h["store"]["transactions"], h["rules"]
                );
                println!("  CA            {}", h["ca_path"].as_str().unwrap_or("?"));
            }
        }
        ("search", _) => {
            let query = args.positional.join(" ");
            let limit: u64 = args
                .opts
                .get("limit")
                .map(|l| l.parse())
                .transpose()?
                .unwrap_or(50);
            let rows = c
                .call("traffic.search", json!({"query": query, "limit": limit}))
                .await?;
            for r in rows.as_array().cloned().unwrap_or_default() {
                if args.flags.contains("json") {
                    println!("{}", r["transaction"]);
                } else {
                    let t: Transaction = serde_json::from_value(r["transaction"].clone())?;
                    println!("{}", one_line(&t));
                }
            }
        }
        ("tail", _) => {
            let query = args.positional.join(" ");
            c.call("traffic.subscribe", json!({"query": query})).await?;
            eprintln!(
                "watching{} — Ctrl-C to stop",
                if query.is_empty() {
                    String::new()
                } else {
                    format!(" `{query}`")
                }
            );
            while let Some(t) = c.next_event().await? {
                if t.state != TransactionState::InProgress {
                    println!("{}", one_line(&t));
                }
            }
        }
        ("rules", "push") => {
            let file = args
                .positional
                .get(1)
                .context("usage: relay rules push <file>")?;
            let r = c
                .call("rules.set", json!({"yaml": std::fs::read_to_string(file)?}))
                .await?;
            if r["saved"].as_bool() == Some(true) {
                println!("rules saved");
            } else {
                for p in r["problems"].as_array().cloned().unwrap_or_default() {
                    println!("{file}: {}", p.as_str().unwrap_or(""));
                }
                std::process::exit(1);
            }
        }
        ("rules", "pull") => print!(
            "{}",
            c.call("rules.get", Value::Null).await?["yaml"]
                .as_str()
                .unwrap_or("")
        ),
        ("proxy", "start") => {
            let port: u16 = args
                .opts
                .get("port")
                .map(|p| p.parse())
                .transpose()?
                .unwrap_or(8888);
            let r = c.call("proxy.start", json!({"port": port})).await?;
            println!("proxy listening on {}", r["port"]);
        }
        ("proxy", "stop") => {
            c.call("proxy.stop", Value::Null).await?;
            println!("proxy stopped");
        }
        ("devices", "") | ("devices", "list") => {
            let r = c.call("devices.list", Value::Null).await?;
            if args.flags.contains("json") {
                println!("{}", pretty(&r));
            } else {
                let list = r["devices"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!(
                        "no simulators or Android devices found (boot one, or plug in a phone with USB debugging)"
                    );
                }
                for d in &list {
                    println!(
                        "{:<38} {:<18} {:<14} {}{}",
                        d["id"].as_str().unwrap_or(""),
                        d["name"].as_str().unwrap_or(""),
                        d["os"].as_str().unwrap_or(""),
                        d["kind"].as_str().unwrap_or(""),
                        if d["connected"].as_bool() == Some(true) {
                            "  ● connected"
                        } else {
                            ""
                        }
                    );
                }
                println!(
                    "\niPhone/iPad over Wi-Fi: open {} on the phone (scan the QR in the app)",
                    r["wifi_setup_url"].as_str().unwrap_or("")
                );
            }
        }
        ("devices", "connect") => {
            let id = args
                .positional
                .get(1)
                .context("usage: relay devices connect <id>")?;
            let r = c.call("devices.connect", json!({"id": id})).await?;
            let mark = |s: &Value| match s["state"].as_str() {
                Some("done") => "✓",
                Some("needs_action") => "!",
                _ => "✗",
            };
            let d = &r["device"];
            println!(
                "{} ({})",
                d["name"].as_str().unwrap_or(""),
                d["os"].as_str().unwrap_or("")
            );
            for (label, key) in [("Proxy", "proxy"), ("Certificate", "certificate")] {
                let s = &r["checklist"][key];
                println!(
                    "  {} {label}: {}",
                    mark(s),
                    s["detail"].as_str().unwrap_or("")
                );
            }
            if r["ready"].as_bool() != Some(true) {
                std::process::exit(2);
            }
        }
        ("devices", "disconnect") => {
            let id = args
                .positional
                .get(1)
                .context("usage: relay devices disconnect <id>")?;
            c.call("devices.disconnect", json!({"id": id})).await?;
            println!("disconnected");
        }
        ("system-proxy", "on") => println!(
            "{}",
            pretty(&c.call("system_proxy.enable", Value::Null).await?)
        ),
        ("system-proxy", "off") => println!(
            "{}",
            pretty(&c.call("system_proxy.disable", Value::Null).await?)
        ),
        ("system-proxy", _) => println!(
            "{}",
            pretty(&c.call("system_proxy.status", Value::Null).await?)
        ),
        (cmd, sub) => bail!("unknown command `{cmd} {sub}`"),
    }
    Ok(())
}

fn one_line(t: &Transaction) -> String {
    let status = match (t.state, t.status) {
        (TransactionState::InProgress, _) => "…".to_string(),
        (TransactionState::Tunneled, _) => "TUNNEL".to_string(),
        (TransactionState::Failed, _) => "FAILED".to_string(),
        (_, Some(s)) => s.to_string(),
        _ => "-".to_string(),
    };
    let lock = if t.decrypted {
        "🔓"
    } else if t.scheme == "https" {
        "🔒"
    } else {
        "  "
    };
    let mut line = format!(
        "{:>6} {lock} {:<7} {:<70} {:>6}ms {:>8}B",
        status,
        t.method,
        truncate(&t.url(), 70),
        t.duration_ms,
        t.response_body_size
    );
    if let Some(e) = &t.error {
        line.push_str(&format!("  — {e}"));
    }
    line
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
