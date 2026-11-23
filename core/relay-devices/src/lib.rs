//! relay-devices: find iOS Simulators, Android emulators and USB phones, and connect them to Relay
//! in one step. Every shell command goes through [`Runner`], so the logic is tested with a fake.
//!
//! What "connect" does:
//! - **iOS Simulator**: trusts the Relay CA in that simulator (`simctl keychain add-root-cert`) and
//!   points the Mac's system proxy at Relay (simulators use the Mac's network settings).
//! - **Android emulator / USB device**: `adb reverse` so the phone reaches Relay on its own
//!   `127.0.0.1` (no shared Wi-Fi, never a changing IP), sets the device's global HTTP proxy, and
//!   installs the CA — into the system store on rootable emulators, otherwise it opens the system
//!   installer and tells you what to tap.
//! - **iPhone / iPad over Wi-Fi**: not auto-detectable; [`wifi_setup_url`] gives the page the QR
//!   code points at.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// The Mac's system proxy, as seen by this crate (implemented by relay-control's `SystemProxy`).
pub trait SystemProxySwitch: Send + Sync {
    fn enable(&self, host: &str, port: u16) -> Result<()>;
    fn disable(&self) -> Result<()>;
}

// ---------- running commands ----------

#[derive(Debug, Clone)]
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait Runner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<Output>;
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        let out = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {program}"))?;
        Ok(Output {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Finds `adb`: PATH first, then the usual SDK locations.
pub fn find_adb() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("adb")).collect())
        .unwrap_or_default();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(h) = std::env::var_os(var) {
            candidates.push(PathBuf::from(h).join("platform-tools/adb"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join("Library/Android/sdk/platform-tools/adb"));
        candidates.push(PathBuf::from(&home).join("Android/Sdk/platform-tools/adb"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

const CERT_FILE_ON_DEVICE: &str = "/sdcard/Download/relay-ca.crt";

/// Android 11+ only installs CA certificates from Settings (Chrome says so too), and Settings reads
/// the file through the system picker. Opening the file directly with a `file://` link fails with
/// "the certificate file couldn't be read", which is what this replaces.
const CERT_GUIDE: &str = "Relay saved relay-ca.crt in the phone's Downloads and opened Settings. Install it from there: Settings → Security → Encryption & credentials → Install a certificate → CA certificate → Install anyway → pick relay-ca.crt in Downloads. On Samsung: Settings → Security and privacy → More security settings → Install from device storage → CA certificate. A screen lock is required. Apps that ignore user certificates also need relay-debug-lib.";

// ---------- model ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    IosSimulator,
    AndroidEmulator,
    AndroidUsb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Simulator UDID or adb serial.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// "iOS 26.4", "Android 15".
    pub os: String,
    /// Whether the device is currently connected to Relay.
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Done(String),
    /// Relay did what it can; the person must finish (e.g. tap Install).
    NeedsAction(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checklist {
    pub proxy: StepState,
    pub certificate: StepState,
}

impl Checklist {
    pub fn ready(&self) -> bool {
        matches!(self.proxy, StepState::Done(_)) && matches!(self.certificate, StepState::Done(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResult {
    pub device: Device,
    pub checklist: Checklist,
    /// Milliseconds since the epoch; the UI counts traffic after this for "Traffic seen".
    pub connected_at_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------- the manager ----------

pub struct DeviceManager {
    runner: Arc<dyn Runner>,
    adb: Option<String>,
    system_proxy: Option<Arc<dyn SystemProxySwitch>>,
    ca_path: PathBuf,
    connected: Mutex<HashMap<String, (u16, u64, Checklist)>>,
    /// Android devices whose proxy Relay set and has not yet undone (serial → port). Kept on disk so
    /// a device unplugged while connected is cleaned up the next time Relay sees it, instead of
    /// being left with a proxy pointing at nothing (no internet).
    pending_cleanup: Mutex<HashMap<String, u16>>,
    state_file: Option<PathBuf>,
}

impl DeviceManager {
    pub fn new(
        runner: Arc<dyn Runner>,
        adb: Option<String>,
        system_proxy: Option<Arc<dyn SystemProxySwitch>>,
        ca_path: &Path,
    ) -> Self {
        Self {
            runner,
            adb,
            system_proxy,
            ca_path: ca_path.to_path_buf(),
            connected: Mutex::new(HashMap::new()),
            pending_cleanup: Mutex::new(HashMap::new()),
            state_file: None,
        }
    }

    /// Remembers connected Android devices in `path` so they are cleaned up even after a crash,
    /// a quit, or the device being unplugged.
    pub fn with_state_file(mut self, path: &Path) -> Self {
        let loaded: HashMap<String, u16> = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        *self.pending_cleanup.get_mut().unwrap() = loaded;
        self.state_file = Some(path.to_path_buf());
        self
    }

    fn save_pending(&self) {
        if let Some(path) = &self.state_file {
            let map = self.pending_cleanup.lock().unwrap();
            if let Ok(text) = serde_json::to_string(&*map) {
                let _ = std::fs::write(path, text);
            }
        }
    }

    /// Undoes the proxy on Android devices left connected by an earlier run (now visible again).
    fn clean_up_leftovers(&self, devices: &[Device]) {
        let todo: Vec<(String, u16)> = {
            let pending = self.pending_cleanup.lock().unwrap();
            let connected = self.connected.lock().unwrap();
            devices
                .iter()
                .filter(|d| !connected.contains_key(&d.id))
                .filter_map(|d| pending.get(&d.id).map(|p| (d.id.clone(), *p)))
                .collect()
        };
        for (id, port) in todo {
            let cleared = self
                .adb(
                    &id,
                    &["shell", "settings", "put", "global", "http_proxy", ":0"],
                )
                .map(|o| o.ok)
                .unwrap_or(false);
            let _ = self.adb(&id, &["reverse", "--remove", &format!("tcp:{port}")]);
            if cleared {
                tracing::info!(device = %id, "removed the proxy Relay left on this device earlier");
                self.pending_cleanup.lock().unwrap().remove(&id);
                self.save_pending();
            }
        }
    }

    /// Real commands, auto-detected `adb`.
    pub fn system(system_proxy: Option<Arc<dyn SystemProxySwitch>>, ca_path: &Path) -> Self {
        let mgr = Self::new(
            Arc::new(SystemRunner),
            find_adb().map(|p| p.to_string_lossy().into_owned()),
            system_proxy,
            ca_path,
        );
        match ca_path.parent() {
            Some(dir) => mgr.with_state_file(&dir.join("android-devices.json")),
            None => mgr,
        }
    }

    fn adb(&self, serial: &str, args: &[&str]) -> Result<Output> {
        let adb = self
            .adb
            .as_deref()
            .ok_or_else(|| anyhow!("adb was not found (install Android platform-tools)"))?;
        let mut full = vec!["-s", serial];
        full.extend_from_slice(args);
        self.runner.run(adb, &full)
    }

    /// Booted simulators and attached Android devices.
    pub fn list(&self) -> Vec<Device> {
        let mut out = Vec::new();
        match self.runner.run(
            "/usr/bin/xcrun",
            &["simctl", "list", "devices", "booted", "-j"],
        ) {
            Ok(o) if o.ok => out.extend(parse_simctl(&o.stdout)),
            Ok(o) => tracing::debug!(stderr = %o.stderr.trim(), "simctl unavailable"),
            Err(e) => tracing::debug!(error = %e, "simctl unavailable"),
        }
        if let Some(adb) = &self.adb
            && let Ok(o) = self.runner.run(adb, &["devices", "-l"])
        {
            for mut d in parse_adb_devices(&o.stdout) {
                d.os = self
                    .adb(&d.id, &["shell", "getprop", "ro.build.version.release"])
                    .ok()
                    .map(|o| format!("Android {}", o.stdout.trim()))
                    .unwrap_or_else(|| "Android".into());
                out.push(d);
            }
        }
        self.clean_up_leftovers(&out);
        let connected = self.connected.lock().unwrap();
        for d in &mut out {
            d.connected = connected.contains_key(&d.id);
        }
        out
    }

    pub fn connect(&self, id: &str, port: u16) -> Result<ConnectResult> {
        let device = self
            .list()
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| anyhow!("device {id} is not running or attached"))?;
        let checklist = match device.kind {
            Kind::IosSimulator => self.connect_simulator(&device, port),
            Kind::AndroidEmulator | Kind::AndroidUsb => self.connect_android(&device, port),
        };
        let at = now_ms();
        self.connected
            .lock()
            .unwrap()
            .insert(device.id.clone(), (port, at, checklist.clone()));
        Ok(ConnectResult {
            device: Device {
                connected: true,
                ..device
            },
            checklist,
            connected_at_ms: at,
        })
    }

    pub fn disconnect(&self, id: &str) -> Result<()> {
        let entry = self.connected.lock().unwrap().remove(id);
        let (port, _, _) = entry.ok_or_else(|| anyhow!("{id} is not connected"))?;
        if id.contains('-') && id.len() == 36 {
            // A simulator: the system proxy is shared, so turn it off with the last one.
            let any_sim = self
                .connected
                .lock()
                .unwrap()
                .keys()
                .any(|k| k.len() == 36 && k.contains('-'));
            if !any_sim && let Some(sp) = &self.system_proxy {
                sp.disable()?;
            }
        } else {
            let cleared = self
                .adb(
                    id,
                    &["shell", "settings", "put", "global", "http_proxy", ":0"],
                )
                .map(|o| o.ok)
                .unwrap_or(false);
            let _ = self.adb(id, &["reverse", "--remove", &format!("tcp:{port}")]);
            // Unplugged devices stay on the cleanup list until we can really undo their proxy.
            if cleared {
                self.pending_cleanup.lock().unwrap().remove(id);
                self.save_pending();
            }
        }
        Ok(())
    }

    /// Undo everything on every connected device (call on shutdown).
    pub fn disconnect_all(&self) {
        let ids: Vec<String> = self.connected.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.disconnect(&id);
        }
    }

    pub fn status(&self, id: &str) -> Option<Checklist> {
        self.connected
            .lock()
            .unwrap()
            .get(id)
            .map(|(_, _, c)| c.clone())
    }

    /// The result of an earlier connect, so a screen opened later shows the same checklist.
    pub fn result(&self, id: &str) -> Option<ConnectResult> {
        let (_, at, checklist) = self.connected.lock().unwrap().get(id).cloned()?;
        let device = self.list().into_iter().find(|d| d.id == id)?;
        Some(ConnectResult {
            device: Device {
                connected: true,
                ..device
            },
            checklist,
            connected_at_ms: at,
        })
    }

    // -- iOS Simulator --

    fn connect_simulator(&self, d: &Device, port: u16) -> Checklist {
        let certificate = match self.runner.run(
            "/usr/bin/xcrun",
            &[
                "simctl",
                "keychain",
                &d.id,
                "add-root-cert",
                &self.ca_path.to_string_lossy(),
            ],
        ) {
            Ok(o) if o.ok => StepState::Done("Relay CA trusted in the simulator".into()),
            Ok(o) => StepState::Failed(first_line(&o.stderr)),
            Err(e) => StepState::Failed(e.to_string()),
        };
        let proxy = match &self.system_proxy {
            Some(sp) => match sp.enable("127.0.0.1", port) {
                Ok(()) => StepState::Done(format!(
                    "Mac system proxy → 127.0.0.1:{port} (simulators use it)"
                )),
                Err(e) => StepState::Failed(format!("{e:#}")),
            },
            None => StepState::NeedsAction(format!(
                "Set this Mac's HTTP and HTTPS proxy to 127.0.0.1:{port}"
            )),
        };
        Checklist { proxy, certificate }
    }

    // -- Android --

    fn connect_android(&self, d: &Device, port: u16) -> Checklist {
        // Certificate first: `adb root` restarts adbd on rootable emulators, which drops every
        // `adb reverse` tunnel — so the tunnel must be created after it.
        let certificate = self.install_android_ca(d);
        let p = port.to_string();
        let tcp = format!("tcp:{p}");
        let proxy = match self.adb(&d.id, &["reverse", &tcp, &tcp]) {
            Ok(o) if o.ok => match self.adb(
                &d.id,
                &[
                    "shell",
                    "settings",
                    "put",
                    "global",
                    "http_proxy",
                    &format!("127.0.0.1:{p}"),
                ],
            ) {
                Ok(o) if o.ok => StepState::Done(format!(
                    "adb reverse + device proxy → 127.0.0.1:{p} (no Wi-Fi needed)"
                )),
                Ok(o) => StepState::Failed(first_line(&o.stderr)),
                Err(e) => StepState::Failed(e.to_string()),
            },
            Ok(o) => StepState::Failed(first_line(&format!("{} {}", o.stderr, o.stdout))),
            Err(e) => StepState::Failed(e.to_string()),
        };
        if matches!(proxy, StepState::Done(_)) {
            self.pending_cleanup
                .lock()
                .unwrap()
                .insert(d.id.clone(), port);
            self.save_pending();
        }
        Checklist { proxy, certificate }
    }

    fn install_android_ca(&self, d: &Device) -> StepState {
        let hash = match self.runner.run(
            "/usr/bin/openssl",
            &[
                "x509",
                "-in",
                &self.ca_path.to_string_lossy(),
                "-subject_hash_old",
                "-noout",
            ],
        ) {
            Ok(o) if o.ok && !o.stdout.trim().is_empty() => o.stdout.trim().to_string(),
            _ => {
                return StepState::Failed(
                    "could not compute the certificate hash (openssl)".into(),
                );
            }
        };
        let name = format!("{hash}.0");
        // Already in the system store (from an earlier connect)?
        if let Ok(o) = self.adb(
            &d.id,
            &[
                "shell",
                "ls",
                &format!("/system/etc/security/cacerts/{name}"),
            ],
        ) && o.ok
        {
            return StepState::Done("Relay CA is in the system certificate store".into());
        }
        if self.try_system_install(d, &name).is_ok() {
            return StepState::Done("Relay CA added to the system certificate store".into());
        }
        // Not rootable (Play Store images, real phones): put the file where the Settings picker
        // can find it and open Settings; the person finishes the install.
        let pushed = self
            .adb(
                &d.id,
                &["push", &self.ca_path.to_string_lossy(), CERT_FILE_ON_DEVICE],
            )
            .map(|o| o.ok)
            .unwrap_or(false);
        if !pushed {
            return StepState::Failed(
                "could not copy the certificate to the phone (is USB file access allowed?)".into(),
            );
        }
        let _ = self.adb(
            &d.id,
            &[
                "shell",
                "am",
                "start",
                "-a",
                "android.settings.SECURITY_SETTINGS",
            ],
        );
        StepState::NeedsAction(CERT_GUIDE.into())
    }

    /// Rootable emulators: overlay the system CA directory with a tmpfs that also holds our CA.
    /// Android 14+ reads CAs from the conscrypt APEX, so that path is overlaid too.
    fn try_system_install(&self, d: &Device, name: &str) -> Result<()> {
        let root = self.runner.run(
            self.adb.as_deref().ok_or_else(|| anyhow!("no adb"))?,
            &["-s", &d.id, "root"],
        )?;
        if !root.ok
            || root.stdout.contains("cannot run as root")
            || root.stderr.contains("cannot run as root")
        {
            return Err(anyhow!("this build does not allow adb root"));
        }
        let _ = self.adb(&d.id, &["wait-for-device"]);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let tmp = "/data/local/tmp/relay-ca.pem";
        if !self
            .adb(&d.id, &["push", &self.ca_path.to_string_lossy(), tmp])?
            .ok
        {
            return Err(anyhow!("push failed"));
        }
        let script = format!(
            "set -e; S=/system/etc/security/cacerts; W=/data/local/tmp/relay-cacerts; rm -rf $W; mkdir -p $W; \
             cp $S/* $W/; cp {tmp} $W/{name}; chmod 644 $W/*; chown root:root $W/*; \
             mount -t tmpfs tmpfs $S; cp $W/* $S/; chmod 644 $S/*; chown root:root $S/*; chcon u:object_r:system_file:s0 $S $S/*; \
             if [ -d /apex/com.android.conscrypt/cacerts ]; then \
               mount --bind $S /apex/com.android.conscrypt/cacerts; \
               for z in $(pidof zygote zygote64); do nsenter --mount=/proc/$z/ns/mnt -- /bin/mount --bind $S /apex/com.android.conscrypt/cacerts; done; \
             fi"
        );
        let o = self.adb(&d.id, &["shell", &script])?;
        if !o.ok {
            return Err(anyhow!(first_line(&o.stderr)));
        }
        Ok(())
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_string()
}

// ---------- parsing ----------

pub fn parse_simctl(json: &str) -> Vec<Device> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (runtime, list) in v["devices"].as_object().into_iter().flatten() {
        // "com.apple.CoreSimulator.SimRuntime.iOS-26-4" → "iOS 26.4"
        let tail = runtime.rsplit('.').next().unwrap_or(runtime);
        let (platform, ver) = tail.split_once('-').unwrap_or((tail, ""));
        if platform != "iOS" && platform != "iPadOS" {
            continue;
        }
        let os = format!("{platform} {}", ver.replace('-', "."));
        for d in list.as_array().into_iter().flatten() {
            if d["state"].as_str() != Some("Booted") {
                continue;
            }
            if let (Some(udid), Some(name)) = (d["udid"].as_str(), d["name"].as_str()) {
                out.push(Device {
                    id: udid.into(),
                    kind: Kind::IosSimulator,
                    name: name.into(),
                    os: os.clone(),
                    connected: false,
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `adb devices -l`: `emulator-5554 device product:… model:Pixel_9_Pro … transport_id:1`.
pub fn parse_adb_devices(text: &str) -> Vec<Device> {
    text.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            if parts.next()? != "device" {
                return None; // offline / unauthorized
            }
            let model = parts
                .find_map(|p| p.strip_prefix("model:"))
                .unwrap_or(serial)
                .replace('_', " ");
            let kind = if serial.starts_with("emulator-") {
                Kind::AndroidEmulator
            } else {
                Kind::AndroidUsb
            };
            Some(Device {
                id: serial.into(),
                kind,
                name: model,
                os: "Android".into(),
                connected: false,
            })
        })
        .collect()
}

/// `http://<LocalHostName>.local:<port>/setup` — what the iPhone QR code encodes. The `.local`
/// name survives the Mac's IP changing.
pub fn wifi_setup_url(runner: &dyn Runner, port: u16) -> String {
    let host = runner
        .run("/usr/sbin/scutil", &["--get", "LocalHostName"])
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout.trim().to_string())
        .filter(|h| !h.is_empty())
        .map(|h| format!("{h}.local"))
        .unwrap_or_else(|| "localhost".into());
    format!("http://{host}:{port}/setup")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every command and answers from a script (first rule whose text is contained wins).
    struct Fake {
        log: Mutex<Vec<String>>,
        rules: Vec<(&'static str, Output)>,
    }

    fn ok(s: &str) -> Output {
        Output {
            ok: true,
            stdout: s.into(),
            stderr: String::new(),
        }
    }
    fn fail(s: &str) -> Output {
        Output {
            ok: false,
            stdout: String::new(),
            stderr: s.into(),
        }
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
            let line = format!("{program} {}", args.join(" "));
            self.log.lock().unwrap().push(line.clone());
            Ok(self
                .rules
                .iter()
                .find(|(k, _)| line.contains(k))
                .map(|(_, o)| o.clone())
                .unwrap_or_else(|| ok("")))
        }
    }

    impl Fake {
        fn new(rules: Vec<(&'static str, Output)>) -> Arc<Fake> {
            Arc::new(Fake {
                log: Mutex::new(vec![]),
                rules,
            })
        }
        fn saw(&self, needle: &str) -> bool {
            self.log.lock().unwrap().iter().any(|l| l.contains(needle))
        }
    }

    const SIMCTL: &str = r#"{"devices":{
      "com.apple.CoreSimulator.SimRuntime.iOS-26-4":[
        {"udid":"6507CF9D-A8F8-4285-96CF-53780AE0EB92","name":"iPhone 17 Pro","state":"Booted"},
        {"udid":"AAAAAAAA-0000-0000-0000-000000000000","name":"iPhone 17","state":"Shutdown"}],
      "com.apple.CoreSimulator.SimRuntime.watchOS-27-0":[
        {"udid":"WWWWWWWW-0000-0000-0000-000000000000","name":"Watch","state":"Booted"}]}}"#;

    const ADB: &str = "List of devices attached\nemulator-5554          device product:sdk model:Pixel_9_Pro device:emu64a transport_id:1\n\
                       R5CT123ABC             device usb:1-1 product:b0q model:SM_S908B transport_id:2\n\
                       0123456789             unauthorized usb:1-2 transport_id:3\n\n";

    #[test]
    fn parses_only_booted_ios_simulators() {
        let d = parse_simctl(SIMCTL);
        assert_eq!(
            d.len(),
            1,
            "shutdown simulators and watchOS are not offered"
        );
        assert_eq!(
            (d[0].name.as_str(), d[0].os.as_str(), d[0].kind),
            ("iPhone 17 Pro", "iOS 26.4", Kind::IosSimulator)
        );
    }

    #[test]
    fn parses_adb_emulators_usb_phones_and_skips_unauthorized() {
        let d = parse_adb_devices(ADB);
        assert_eq!(d.len(), 2);
        assert_eq!(
            (d[0].kind, d[0].name.as_str()),
            (Kind::AndroidEmulator, "Pixel 9 Pro")
        );
        assert_eq!(
            (d[1].kind, d[1].id.as_str()),
            (Kind::AndroidUsb, "R5CT123ABC")
        );
    }

    #[derive(Default)]
    struct FakeSwitch(Mutex<Vec<String>>);
    impl SystemProxySwitch for FakeSwitch {
        fn enable(&self, host: &str, port: u16) -> Result<()> {
            self.0.lock().unwrap().push(format!("on {host}:{port}"));
            Ok(())
        }
        fn disable(&self) -> Result<()> {
            self.0.lock().unwrap().push("off".into());
            Ok(())
        }
    }

    #[test]
    fn connecting_a_simulator_trusts_the_ca_and_sets_the_system_proxy() {
        let fake = Fake::new(vec![("simctl list", ok(SIMCTL))]);
        let sw = Arc::new(FakeSwitch::default());
        let m = DeviceManager::new(
            fake.clone(),
            None,
            Some(sw.clone()),
            Path::new("/tmp/relay-ca.pem"),
        );
        let id = "6507CF9D-A8F8-4285-96CF-53780AE0EB92";
        let r = m.connect(id, 8888).unwrap();
        assert!(r.checklist.ready(), "{:?}", r.checklist);
        assert!(fake.saw(&format!(
            "simctl keychain {id} add-root-cert /tmp/relay-ca.pem"
        )));
        assert_eq!(*sw.0.lock().unwrap(), vec!["on 127.0.0.1:8888"]);
        assert!(m.list()[0].connected);
        m.disconnect(id).unwrap();
        assert_eq!(
            *sw.0.lock().unwrap(),
            vec!["on 127.0.0.1:8888", "off"],
            "last simulator turns the proxy off"
        );
        assert!(!m.list()[0].connected);
    }

    #[test]
    fn a_failed_certificate_step_is_reported_not_hidden() {
        let fake = Fake::new(vec![
            ("simctl list", ok(SIMCTL)),
            ("add-root-cert", fail("Invalid device state")),
        ]);
        let m = DeviceManager::new(fake, None, None, Path::new("/tmp/ca.pem"));
        let r = m
            .connect("6507CF9D-A8F8-4285-96CF-53780AE0EB92", 8888)
            .unwrap();
        assert_eq!(
            r.checklist.certificate,
            StepState::Failed("Invalid device state".into())
        );
        assert!(
            matches!(r.checklist.proxy, StepState::NeedsAction(_)),
            "no system proxy manager → tell the person"
        );
        assert!(!r.checklist.ready());
    }

    #[test]
    fn android_uses_adb_reverse_and_the_device_proxy_then_undoes_both() {
        let fake = Fake::new(vec![
            ("devices -l", ok(ADB)),
            ("getprop ro.build.version.release", ok("15\n")),
            ("subject_hash_old", ok("eadedf90\n")),
            (
                "ls /system/etc/security/cacerts/eadedf90.0",
                fail("No such file"),
            ),
            (
                " root",
                fail("adbd cannot run as root in production builds"),
            ),
        ]);
        let m = DeviceManager::new(
            fake.clone(),
            Some("adb".into()),
            None,
            Path::new("/tmp/ca.pem"),
        );
        let r = m.connect("emulator-5554", 8888).unwrap();
        assert!(fake.saw("-s emulator-5554 reverse tcp:8888 tcp:8888"));
        assert!(fake.saw("settings put global http_proxy 127.0.0.1:8888"));
        assert!(matches!(r.checklist.proxy, StepState::Done(_)));
        assert!(
            matches!(r.checklist.certificate, StepState::NeedsAction(_)),
            "non-rootable → guided install"
        );
        assert!(
            !fake.saw("file://"),
            "Android's installer cannot read file:// paths"
        );
        assert!(fake.saw("push /tmp/ca.pem /sdcard/Download/relay-ca.crt"));
        assert!(fake.saw("android.settings.SECURITY_SETTINGS"));
        assert!(
            matches!(&r.checklist.certificate, StepState::NeedsAction(t) if t.contains("Install a certificate") && t.contains("Samsung")),
            "the person gets the real steps, stock and Samsung"
        );
        m.disconnect("emulator-5554").unwrap();
        assert!(fake.saw("http_proxy :0") && fake.saw("reverse --remove tcp:8888"));
    }

    #[test]
    fn rootable_emulators_get_the_ca_in_the_system_store() {
        let fake = Fake::new(vec![
            ("devices -l", ok(ADB)),
            ("subject_hash_old", ok("eadedf90\n")),
            (
                "ls /system/etc/security/cacerts/eadedf90.0",
                fail("No such file"),
            ),
            (" root", ok("restarting adbd as root\n")),
        ]);
        let m = DeviceManager::new(
            fake.clone(),
            Some("adb".into()),
            None,
            Path::new("/tmp/ca.pem"),
        );
        let r = m.connect("emulator-5554", 8888).unwrap();
        assert!(
            matches!(r.checklist.certificate, StepState::Done(_)),
            "{:?}",
            r.checklist.certificate
        );
        assert!(fake.saw("mount -t tmpfs tmpfs") && fake.saw("eadedf90"));
        let log = fake.log.lock().unwrap();
        let at = |needle: &str| {
            log.iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle}"))
        };
        assert!(
            at(" root") < at("reverse tcp:8888"),
            "the reverse tunnel must come after adbd restarts"
        );
        assert!(at("mount -t tmpfs") < at("http_proxy 127.0.0.1:8888"));
        drop(log);
        assert!(
            fake.saw("/apex/com.android.conscrypt/cacerts"),
            "Android 14+ path is overlaid too"
        );
    }

    #[test]
    fn a_device_unplugged_while_connected_is_cleaned_up_when_it_comes_back() {
        let dir = std::env::temp_dir().join(format!("relay-dev-leftover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("android-devices.json");

        // Connect, then the cable is pulled: adb can no longer reach the phone.
        let present = Fake::new(vec![
            ("devices -l", ok(ADB)),
            ("subject_hash_old", ok("eadedf90\n")),
            (
                " root",
                fail("adbd cannot run as root in production builds"),
            ),
        ]);
        let m = DeviceManager::new(present, Some("adb".into()), None, Path::new("/tmp/ca.pem"))
            .with_state_file(&state);
        m.connect("R5CT123ABC", 8888).unwrap();
        assert!(
            std::fs::read_to_string(&state)
                .unwrap()
                .contains("R5CT123ABC"),
            "remembered on disk"
        );
        drop(m); // Relay quits / crashes with the phone's proxy still set

        // Next run: nothing connected yet, but the phone is plugged in again.
        let again = Fake::new(vec![("devices -l", ok(ADB))]);
        let m2 = DeviceManager::new(
            again.clone(),
            Some("adb".into()),
            None,
            Path::new("/tmp/ca.pem"),
        )
        .with_state_file(&state);
        assert!(!again.saw("http_proxy :0"));
        let seen = m2.list();
        assert!(seen.iter().any(|d| d.id == "R5CT123ABC" && !d.connected));
        assert!(
            again.saw("-s R5CT123ABC shell settings put global http_proxy :0"),
            "leftover proxy removed"
        );
        assert!(again.saw("reverse --remove tcp:8888"));
        assert!(
            !std::fs::read_to_string(&state)
                .unwrap()
                .contains("R5CT123ABC"),
            "forgotten once cleaned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_devices_are_a_clear_error() {
        let m = DeviceManager::new(Fake::new(vec![]), None, None, Path::new("/tmp/ca.pem"));
        assert!(
            m.connect("nope", 8888)
                .unwrap_err()
                .to_string()
                .contains("not running or attached")
        );
    }

    #[test]
    fn wifi_setup_url_uses_the_bonjour_name() {
        let fake = Fake::new(vec![("LocalHostName", ok("Jayanths-MacBook-Pro-2\n"))]);
        assert_eq!(
            wifi_setup_url(fake.as_ref(), 8888),
            "http://Jayanths-MacBook-Pro-2.local:8888/setup"
        );
    }
}
