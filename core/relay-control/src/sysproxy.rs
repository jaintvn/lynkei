//! macOS system proxy control with crash safety.
//!
//! The original settings are written to `system-proxy.json` BEFORE anything changes, so they can
//! always be put back: by `disable()`, by `recover()` when relayd starts after a crash, and by the
//! guard process (`relayd guard`) that watches relayd and restores the settings if it dies —
//! including `kill -9`. This fixes Charles's classic "no internet after it crashed".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const STATE_FILE: &str = "system-proxy.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

/// The HTTP and HTTPS proxy settings of one network service.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySettings {
    pub web: Endpoint,
    pub secure: Endpoint,
    pub bypass: Vec<String>,
}

pub trait SystemProxyBackend: Send + Sync {
    /// The network service carrying the default route (e.g. "Wi-Fi").
    fn active_service(&self) -> Result<String>;
    fn read(&self, service: &str) -> Result<ProxySettings>;
    fn write(&self, service: &str, settings: &ProxySettings) -> Result<()>;
}

/// What Relay changed, persisted so it can be undone even after a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub service: String,
    pub original: ProxySettings,
    pub ours: ProxySettings,
    /// relayd's pid when it made the change (the guard watches it).
    pub pid: u32,
}

pub struct SystemProxy {
    backend: Arc<dyn SystemProxyBackend>,
    state_file: PathBuf,
    saved: Mutex<Option<Saved>>,
}

impl SystemProxy {
    /// Recovers any settings left behind by a crash, then returns a manager with nothing applied.
    pub fn new(backend: Arc<dyn SystemProxyBackend>, data_dir: &Path) -> Result<SystemProxy> {
        let state_file = data_dir.join(STATE_FILE);
        recover(backend.as_ref(), &state_file)?;
        Ok(SystemProxy {
            backend,
            state_file,
            saved: Mutex::new(None),
        })
    }

    pub fn enable(&self, host: &str, port: u16) -> Result<()> {
        let mut saved = self.saved.lock().unwrap();
        let (service, original) = match saved.as_ref() {
            Some(s) => (s.service.clone(), s.original.clone()),
            None => {
                let service = self.backend.active_service()?;
                let original = self.backend.read(&service)?;
                (service, original)
            }
        };
        let mut bypass = original.bypass.clone();
        for b in ["localhost", "127.0.0.1", "*.local", "169.254/16"] {
            if !bypass.iter().any(|x| x == b) {
                bypass.push(b.to_string());
            }
        }
        let ours = ProxySettings {
            web: Endpoint {
                enabled: true,
                host: host.into(),
                port,
            },
            secure: Endpoint {
                enabled: true,
                host: host.into(),
                port,
            },
            bypass,
        };
        let record = Saved {
            service: service.clone(),
            original,
            ours: ours.clone(),
            pid: std::process::id(),
        };
        // Persist first: a crash between these two lines must still be recoverable.
        crate::engine::write_atomic(
            &self.state_file,
            serde_json::to_string_pretty(&record)?.as_bytes(),
        )?;
        if let Err(e) = self.backend.write(&service, &ours) {
            let _ = std::fs::remove_file(&self.state_file);
            return Err(e);
        }
        *saved = Some(record);
        Ok(())
    }

    pub fn disable(&self) -> Result<()> {
        let mut saved = self.saved.lock().unwrap();
        if saved.take().is_some() {
            recover(self.backend.as_ref(), &self.state_file)?;
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.saved.lock().unwrap().is_some()
    }

    pub fn state_file(&self) -> &Path {
        &self.state_file
    }

    pub fn status_json(&self) -> Value {
        match self.saved.lock().unwrap().as_ref() {
            Some(s) => {
                json!({"enabled": true, "service": s.service, "host": s.ours.web.host, "port": s.ours.web.port})
            }
            None => json!({"enabled": false}),
        }
    }
}

/// Puts back the settings recorded in `state_file`, if any. Leaves them alone if someone changed
/// the proxy since Relay set it (so a user's own change is never clobbered). Returns whether a
/// restore happened.
pub fn recover(backend: &dyn SystemProxyBackend, state_file: &Path) -> Result<bool> {
    let text = match std::fs::read_to_string(state_file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let saved: Saved = serde_json::from_str(&text).context("reading saved system proxy state")?;
    let now = backend.read(&saved.service)?;
    let still_ours = now.web == saved.ours.web && now.secure == saved.ours.secure;
    if still_ours {
        backend.write(&saved.service, &saved.original)?;
    } else {
        tracing::warn!(service = %saved.service, "system proxy was changed by someone else; leaving it as is");
    }
    std::fs::remove_file(state_file).ok();
    Ok(still_ours)
}

/// The guard loop (`relayd guard`): runs for relayd's lifetime in its own process. When relayd
/// (`pid`) is gone — clean exit, crash or `kill -9` — it restores anything left in `state_file`
/// and exits. Returns whether a restore was needed.
pub fn guard(
    backend: &dyn SystemProxyBackend,
    state_file: &Path,
    pid: u32,
    poll: std::time::Duration,
) -> Result<bool> {
    while process_alive(pid) {
        std::thread::sleep(poll);
    }
    recover(backend, state_file)
}

pub fn process_alive(pid: u32) -> bool {
    // Signal 0 checks existence without sending anything. EPERM still means it exists.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Real macOS backend: `/usr/sbin/networksetup` (works without root for admin users).
pub struct Networksetup;

impl Networksetup {
    fn run(args: &[&str]) -> Result<String> {
        let out = Command::new("/usr/sbin/networksetup")
            .args(args)
            .output()
            .context("running networksetup")?;
        if !out.status.success() {
            // networksetup reports most errors on stdout, so show whichever has text.
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = String::from_utf8_lossy(&out.stdout);
            let why = [err.trim(), msg.trim()]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or("no message; usually this account may not change network settings (managed Mac)");
            bail!("networksetup {} failed: {why}", args.join(" "));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn parse_endpoint(text: &str) -> Endpoint {
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()))
                .unwrap_or_default()
        };
        Endpoint {
            enabled: field("Enabled:") == "Yes",
            host: field("Server:"),
            port: field("Port:").parse().unwrap_or(0),
        }
    }

    fn write_endpoint(service: &str, kind: &str, e: &Endpoint) -> Result<()> {
        if e.enabled || !e.host.is_empty() {
            Self::run(&[
                &format!("-set{kind}proxy"),
                service,
                &e.host,
                &e.port.to_string(),
            ])?;
        }
        Self::run(&[
            &format!("-set{kind}proxystate"),
            service,
            if e.enabled { "on" } else { "off" },
        ])?;
        Ok(())
    }
}

impl SystemProxyBackend for Networksetup {
    fn active_service(&self) -> Result<String> {
        let route = Command::new("/sbin/route")
            .args(["-n", "get", "default"])
            .output()?;
        let iface = String::from_utf8_lossy(&route.stdout)
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("interface:")
                    .map(|s| s.trim().to_string())
            })
            .context("no default route")?;
        let ports = Self::run(&["-listallhardwareports"])?;
        let mut current = None;
        for line in ports.lines() {
            if let Some(p) = line.strip_prefix("Hardware Port: ") {
                current = Some(p.to_string());
            } else if line.strip_prefix("Device: ").map(str::trim) == Some(iface.as_str()) {
                return current.context("hardware port without a name");
            }
        }
        bail!("no network service for interface {iface}")
    }

    fn read(&self, service: &str) -> Result<ProxySettings> {
        let bypass = Self::run(&["-getproxybypassdomains", service])?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("There aren't any"))
            .map(String::from)
            .collect();
        Ok(ProxySettings {
            web: Self::parse_endpoint(&Self::run(&["-getwebproxy", service])?),
            secure: Self::parse_endpoint(&Self::run(&["-getsecurewebproxy", service])?),
            bypass,
        })
    }

    fn write(&self, service: &str, s: &ProxySettings) -> Result<()> {
        Self::write_endpoint(service, "web", &s.web)?;
        Self::write_endpoint(service, "secureweb", &s.secure)?;
        let mut args = vec!["-setproxybypassdomains", service];
        let empty = "Empty".to_string();
        if s.bypass.is_empty() {
            args.push(&empty);
        } else {
            args.extend(s.bypass.iter().map(String::as_str));
        }
        Self::run(&args)?;
        Ok(())
    }
}

/// A backend that keeps "system" settings in a JSON file. For tests and CI: lets the whole
/// crash-recovery path (including `kill -9`) run without touching the machine's real network.
pub struct FileBackend {
    pub path: PathBuf,
}

impl SystemProxyBackend for FileBackend {
    fn active_service(&self) -> Result<String> {
        Ok("Test Wi-Fi".into())
    }
    fn read(&self, _service: &str) -> Result<ProxySettings> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => Ok(serde_json::from_str(&t)?),
            Err(_) => Ok(ProxySettings::default()),
        }
    }
    fn write(&self, _service: &str, s: &ProxySettings) -> Result<()> {
        crate::engine::write_atomic(&self.path, serde_json::to_string_pretty(s)?.as_bytes())
    }
}

impl relay_devices::SystemProxySwitch for SystemProxy {
    fn enable(&self, host: &str, port: u16) -> Result<()> {
        SystemProxy::enable(self, host, port)
    }
    fn disable(&self) -> Result<()> {
        SystemProxy::disable(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str) -> (PathBuf, Arc<FileBackend>, ProxySettings) {
        let dir =
            std::env::temp_dir().join(format!("relay-sysproxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = Arc::new(FileBackend {
            path: dir.join("system.json"),
        });
        let original = ProxySettings {
            web: Endpoint {
                enabled: false,
                host: String::new(),
                port: 0,
            },
            secure: Endpoint {
                enabled: true,
                host: "corp-proxy.example".into(),
                port: 3128,
            },
            bypass: vec!["*.local".into(), "169.254/16".into()],
        };
        backend.write("Test Wi-Fi", &original).unwrap();
        (dir, backend, original)
    }

    #[test]
    fn enable_then_disable_restores_exactly() {
        let (dir, backend, original) = setup("roundtrip");
        let sp = SystemProxy::new(backend.clone(), &dir).unwrap();
        sp.enable("127.0.0.1", 8888).unwrap();
        let now = backend.read("Test Wi-Fi").unwrap();
        assert_eq!(
            (now.web.port, now.secure.host.as_str()),
            (8888, "127.0.0.1")
        );
        assert!(
            now.bypass.contains(&"localhost".to_string())
                && now.bypass.contains(&"*.local".to_string())
        );
        assert!(
            sp.state_file().exists(),
            "original saved before changing anything"
        );
        sp.disable().unwrap();
        assert_eq!(
            backend.read("Test Wi-Fi").unwrap(),
            original,
            "corporate proxy and bypass list come back"
        );
        assert!(!sp.state_file().exists());
    }

    #[test]
    fn crash_is_recovered_on_next_start() {
        let (dir, backend, original) = setup("crash");
        let sp = SystemProxy::new(backend.clone(), &dir).unwrap();
        sp.enable("127.0.0.1", 8888).unwrap();
        drop(sp); // relayd "crashed" without disabling
        assert_eq!(backend.read("x").unwrap().web.port, 8888);
        let _sp = SystemProxy::new(backend.clone(), &dir).unwrap();
        assert_eq!(
            backend.read("x").unwrap(),
            original,
            "next start puts the network back"
        );
    }

    #[test]
    fn a_users_own_change_is_never_clobbered() {
        let (dir, backend, _) = setup("user");
        let sp = SystemProxy::new(backend.clone(), &dir).unwrap();
        sp.enable("127.0.0.1", 8888).unwrap();
        let mine = ProxySettings {
            web: Endpoint {
                enabled: true,
                host: "10.0.0.9".into(),
                port: 9999,
            },
            ..Default::default()
        };
        backend.write("x", &mine).unwrap(); // user switched to another proxy meanwhile
        drop(sp);
        assert!(!recover(backend.as_ref(), &dir.join(STATE_FILE)).unwrap());
        assert_eq!(backend.read("x").unwrap(), mine);
    }

    #[test]
    fn guard_restores_when_the_watched_process_dies() {
        let (dir, backend, original) = setup("guard");
        let sp = SystemProxy::new(backend.clone(), &dir).unwrap();
        sp.enable("127.0.0.1", 8888).unwrap();
        // A pid that cannot exist stands in for a killed relayd.
        let restored = guard(
            backend.as_ref(),
            sp.state_file(),
            999_999,
            std::time::Duration::from_millis(10),
        )
        .unwrap();
        assert!(restored);
        assert_eq!(backend.read("x").unwrap(), original);
    }

    #[test]
    fn parses_networksetup_output() {
        let e = Networksetup::parse_endpoint(
            "Enabled: Yes\nServer: 127.0.0.1\nPort: 8888\nAuthenticated Proxy Enabled: 0\n",
        );
        assert_eq!(
            e,
            Endpoint {
                enabled: true,
                host: "127.0.0.1".into(),
                port: 8888
            }
        );
        let off = Networksetup::parse_endpoint("Enabled: No\nServer: \nPort: 0\n");
        assert!(!off.enabled && off.host.is_empty());
    }
}
