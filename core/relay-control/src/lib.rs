//! relay-control: the one API every Relay surface talks to (Mac app, CLI, MCP, Android remote view).
//!
//! Transport: JSON-RPC 2.0, one JSON object per line, over a Unix socket in the data directory
//! (mode 0600, so only the owning user can connect). Versioned as [`API_VERSION`].
//!
//! | method | params | result |
//! |---|---|---|
//! | `hello` | – | api, engine version, proxy, store, CA, system proxy |
//! | `proxy.start` | `{port?, listen?}` | `{port}` |
//! | `proxy.stop` | – | `{}` |
//! | `traffic.search` | `{query?, limit?}` | `[{key, session, transaction}]` newest first |
//! | `traffic.get` | `{key}` | `{key, session, transaction}` |
//! | `traffic.clear` | – | `{}` |
//! | `traffic.subscribe` | `{query?}` | `{}`, then `traffic.event` notifications `{transaction}` |
//! | `rules.get` | – | `{yaml, problems}` |
//! | `rules.set` | `{yaml}` | `{saved, problems}` (saved only when valid) |
//! | `ca.info` | – | `{path, pem}` |
//! | `system_proxy.status` / `.enable {port?}` / `.disable` | | system proxy state |

pub mod engine;
pub mod server;
pub mod sysproxy;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub const API_VERSION: &str = "relay.control/1";
pub const SOCKET_FILE: &str = "relayd.sock";

pub use engine::{Engine, EngineOptions};

/// `data_dir/relayd.sock`, or — when that path is too long for a Unix socket (about 104 bytes on
/// macOS) — a short path in the temp directory derived from `data_dir`, so the daemon and its
/// clients always agree on it.
pub fn default_socket(data_dir: &Path) -> PathBuf {
    let preferred = data_dir.join(SOCKET_FILE);
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    // FNV-1a: stable across runs and platforms (unlike std's randomized hasher).
    let h = platform::fnv(data_dir.as_os_str().as_encoded_bytes());
    std::env::temp_dir().join(format!("relayd-{h:016x}.sock"))
}

/// Where the control socket lives. macOS/Linux: a 0600 file. Android: SELinux forbids socket files
/// in most places, so an abstract socket named from the path (also what `adb forward
/// localabstract:<name>` reaches). Either way the server checks the peer's uid.
pub mod platform {
    use std::path::Path;
    use tokio::net::UnixStream;

    pub(crate) fn fnv(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in bytes {
            h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
        }
        h
    }

    /// The abstract name used on Android for `socket`.
    pub fn abstract_name(socket: &Path) -> String {
        format!("relayd.{:016x}", fnv(socket.as_os_str().as_encoded_bytes()))
    }

    #[cfg(target_os = "android")]
    pub async fn connect(socket: &Path) -> std::io::Result<UnixStream> {
        use std::os::android::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name(socket))?;
        let s = std::os::unix::net::UnixStream::connect_addr(&addr)?;
        s.set_nonblocking(true)?;
        UnixStream::from_std(s)
    }

    #[cfg(not(target_os = "android"))]
    pub async fn connect(socket: &Path) -> std::io::Result<UnixStream> {
        UnixStream::connect(socket).await
    }

    /// Only our own user may drive the engine; on Android also the adb shell user (2000), which
    /// carries `adb forward` connections from a developer's computer.
    pub fn peer_allowed(stream: &UnixStream) -> bool {
        let me = unsafe { libc::getuid() };
        match stream.peer_cred() {
            Ok(c) => c.uid() == me || (cfg!(target_os = "android") && c.uid() == 2000),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_data_dirs_get_a_short_stable_socket() {
        let short = std::path::Path::new("/Users/a/Library/Application Support/Relay");
        assert_eq!(super::default_socket(short), short.join("relayd.sock"));
        let long = std::path::PathBuf::from(format!("/tmp/{}", "x".repeat(120)));
        let s = super::default_socket(&long);
        assert!(s.as_os_str().len() < 104, "{}", s.display());
        assert_eq!(s, super::default_socket(&long), "same path every time");
        assert_ne!(s, super::default_socket(&long.join("other")));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const ENGINE: i64 = -32000;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// A connected control client.
pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    pub async fn connect(socket: &Path) -> Result<Client> {
        let stream = platform::connect(socket).await.with_context(|| {
            format!(
                "connecting to relayd at {} (is it running? `relayd` starts it)",
                socket.display()
            )
        })?;
        let (r, w) = stream.into_split();
        Ok(Client {
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
        })
    }

    /// Calls `method` and waits for its result, skipping any notifications that arrive first.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.writer.write_all(format!("{req}\n").as_bytes()).await?;
        loop {
            let msg = self
                .next_message()
                .await?
                .context("relayd closed the connection")?;
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let e: RpcError = serde_json::from_value(err.clone())
                    .unwrap_or(RpcError::new(0, err.to_string()));
                bail!(e);
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Next message of any kind (response or notification); `None` when the server closed.
    pub async fn next_message(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_str(&line).context("bad message from relayd")?,
        ))
    }

    /// Next `traffic.event` notification's transaction.
    pub async fn next_event(&mut self) -> Result<Option<relay_model::Transaction>> {
        while let Some(msg) = self.next_message().await? {
            if msg.get("method").and_then(Value::as_str) == Some("traffic.event") {
                let t = msg
                    .pointer("/params/transaction")
                    .cloned()
                    .context("event without transaction")?;
                return Ok(Some(serde_json::from_value(t)?));
            }
        }
        Ok(None)
    }
}
