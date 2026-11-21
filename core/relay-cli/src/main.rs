//! relay — command-line front end for the Relay engine.
//!
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

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| "help".into());
    let (mut positional, mut opts, mut flags) = (vec![], Default::default(), std::collections::HashSet::new());
    let rest: Vec<String> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        if let Some(name) = a.strip_prefix("--") {
            if matches!(name, "json") {
                flags.insert(name.to_string());
            } else if let Some(v) = rest.get(i + 1) {
                let o: &mut std::collections::HashMap<String, String> = &mut opts;
                o.insert(name.to_string(), v.clone());
                i += 1;
            }
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    Args { cmd, positional, opts, flags }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("RELAY_LOG").unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    let args = parse_args();
    let data_dir = args.opts.get("data-dir").map(PathBuf::from).unwrap_or_else(default_data_dir);

    match args.cmd.as_str() {
        "serve" => serve(&args, data_dir).await,
        "fetch" => fetch(&args, data_dir).await,
        "get" => get(&args).await,
        "ca" => {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let ca = relay_proxy::ca::CertAuthority::load_or_create(&data_dir, provider)?;
            println!("{}", ca.ca_path().display());
            Ok(())
        }
        _ => {
            eprintln!("usage:\n  relay get <url> [--ca FILE]\n  relay serve [--port 8888] [--listen 127.0.0.1] [--data-dir DIR] [--json]\n  relay fetch <url> [--proxy 127.0.0.1:8888]\n  relay ca");
            Ok(())
        }
    }
}

async fn serve(args: &Args, data_dir: PathBuf) -> Result<()> {
    let port: u16 = args.opts.get("port").map(|p| p.parse()).transpose()?.unwrap_or(8888);
    let ip = args.opts.get("listen").map(String::as_str).unwrap_or("127.0.0.1");
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
    eprintln!("Devices can also download it from http://{} via the proxy. Ctrl-C to stop.", relay_proxy::CA_DOWNLOAD_HOST);
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn fetch(args: &Args, data_dir: PathBuf) -> Result<()> {
    let Some(url) = args.positional.first() else { bail!("usage: relay fetch <url> [--proxy host:port]") };
    let proxy: SocketAddr = args.opts.get("proxy").map(String::as_str).unwrap_or("127.0.0.1:8888").parse()?;
    let pem = std::fs::read_to_string(data_dir.join(relay_proxy::ca::CA_CERT_FILE))
        .with_context(|| format!("reading Relay CA from {}", data_dir.display()))?;
    use rustls::pki_types::pem::PemObject;
    let ca = CertificateDer::from_pem_slice(pem.as_bytes())?;
    let r = fetch_via_proxy(proxy, url, &[ca]).await?;
    println!("HTTP {} ({} bytes) — TLS verified against the Relay CA only", r.status, r.body.len());
    for (k, v) in r.headers.iter().take(8) {
        println!("  {k}: {v}");
    }
    Ok(())
}

/// Direct GET (no proxy). `--ca FILE` trusts only that CA (proves interception); default trusts the public web PKI.
async fn get(args: &Args) -> Result<()> {
    let Some(url) = args.positional.first() else { bail!("usage: relay get <url> [--ca relay-ca.pem]") };
    let ca = match args.opts.get("ca") {
        Some(path) => {
            use rustls::pki_types::pem::PemObject;
            Some(vec![CertificateDer::from_pem_slice(std::fs::read_to_string(path)?.as_bytes())?])
        }
        None => None,
    };
    let started = std::time::Instant::now();
    let r = relay_proxy::client::fetch_direct(url, ca.as_deref()).await?;
    let secs = started.elapsed().as_secs_f64();
    let trust = if ca.is_some() { "Relay CA only" } else { "public web PKI" };
    println!(
        "HTTP {} — {} bytes in {:.2}s ({:.1} Mbit/s) — TLS trust: {trust}",
        r.status,
        r.body.len(),
        secs,
        r.body.len() as f64 * 8.0 / secs / 1e6
    );
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
    let lock = if t.decrypted { "🔓" } else if t.scheme == "https" { "🔒" } else { "  " };
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
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}
