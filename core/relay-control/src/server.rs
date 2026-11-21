//! Serves the control API on a Unix socket.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use relay_model::filter::Filter;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

use crate::{Engine, RpcError};

/// Binds the control socket (replacing a stale one). See [`crate::platform`] for where it lives.
#[cfg(not(target_os = "android"))]
pub fn bind(socket: &Path) -> Result<UnixListener> {
    if socket.exists() {
        // A live relayd answers; a stale file from a crash does not.
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            anyhow::bail!("another relayd is already running on {}", socket.display());
        }
        std::fs::remove_file(socket).ok();
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(target_os = "android")]
pub fn bind(socket: &Path) -> Result<UnixListener> {
    use std::os::android::net::SocketAddrExt;
    let name = crate::platform::abstract_name(socket);
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(&name)?;
    let std_listener = std::os::unix::net::UnixListener::bind_addr(&addr).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!("another relayd is already running (abstract socket @{name})")
        } else {
            anyhow::anyhow!("binding abstract socket @{name}: {e}")
        }
    })?;
    std_listener.set_nonblocking(true)?;
    Ok(UnixListener::from_std(std_listener)?)
}

/// Accepts connections until the listener fails.
pub async fn serve(listener: UnixListener, engine: Arc<Engine>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if !crate::platform::peer_allowed(&stream) {
                    tracing::warn!(peer = ?stream.peer_cred().ok().map(|c| c.uid()), "refused a control connection from another user");
                    continue;
                }
                let engine = engine.clone();
                tokio::spawn(async move {
                    if let Err(e) = connection(stream, engine).await {
                        tracing::debug!(error = %e, "control connection ended");
                    }
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "control socket accept failed");
                return;
            }
        }
    }
}

async fn connection(stream: UnixStream, engine: Arc<Engine>) -> Result<()> {
    let (r, mut w) = stream.into_split();
    // One writer task, so responses and event notifications never interleave mid-line.
    let (out_tx, mut out_rx) = mpsc::channel::<Value>(1024);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if w.write_all(format!("{msg}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = out_tx
                    .send(error_response(
                        Value::Null,
                        RpcError::new(-32700, format!("parse error: {e}")),
                    ))
                    .await;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        if method == "traffic.subscribe" {
            let query = params.get("query").and_then(Value::as_str).unwrap_or("");
            match Filter::parse(query) {
                Ok(filter) => {
                    let _ = out_tx
                        .send(json!({"jsonrpc": "2.0", "id": id, "result": {}}))
                        .await;
                    tokio::spawn(forward_events(engine.subscribe(), filter, out_tx.clone()));
                }
                Err(e) => {
                    let _ = out_tx
                        .send(error_response(
                            id,
                            RpcError::new(RpcError::INVALID_PARAMS, e.to_string()),
                        ))
                        .await;
                }
            }
            continue;
        }

        let engine = engine.clone();
        let out = out_tx.clone();
        tokio::spawn(async move {
            let msg = match engine.handle(&method, params).await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(e) => error_response(id, e),
            };
            let _ = out.send(msg).await;
        });
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

async fn forward_events(
    mut rx: broadcast::Receiver<relay_model::Transaction>,
    filter: Filter,
    out: mpsc::Sender<Value>,
) {
    loop {
        match rx.recv().await {
            Ok(t) if filter.matches(&t) => {
                let note = json!({"jsonrpc": "2.0", "method": "traffic.event", "params": {"transaction": t}});
                if out.send(note).await.is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(n)) => {
                let note =
                    json!({"jsonrpc": "2.0", "method": "traffic.lagged", "params": {"missed": n}});
                if out.send(note).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn error_response(id: Value, e: RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}})
}
