//! relay-netstack: turns raw IP packets from a TUN device (Android `VpnService`, later the iOS
//! companion) into TCP/UDP streams and feeds TCP into the Relay engine as transparent connections.
//!
//! Routing rule the host must follow: the Relay app itself must NOT be routed into the TUN
//! (Android: `addAllowedApplication(target)` only, or `protect()` every upstream socket), otherwise
//! the engine's own upstream connections would loop back into the VPN.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use ipstack::{IpStack, IpStackConfig, IpStackStream};
use relay_proxy::TransparentHandler;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

#[cfg(unix)]
mod tun_fd;
#[cfg(unix)]
pub use tun_fd::TunFd;

/// Live counters, readable from the UI.
#[derive(Default, Debug)]
pub struct NetstackStats {
    pub tcp_connections: AtomicU64,
    pub udp_flows: AtomicU64,
    pub quic_blocked: AtomicU64,
}

pub struct VpnCapture {
    task: JoinHandle<()>,
    pub stats: Arc<NetstackStats>,
}

impl VpnCapture {
    pub fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for VpnCapture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Debug)]
pub struct NetstackOptions {
    pub mtu: u16,
    /// Drop UDP/443 so apps (Chrome/Cronet, YouTube) fall back from QUIC to TCP, which Relay can decrypt.
    pub block_quic: bool,
    /// Largest single write handed to the user-space TCP stack (0 = unlimited).
    pub max_write: usize,
}

impl Default for NetstackOptions {
    fn default() -> Self {
        Self {
            mtu: 1500,
            block_quic: true,
            max_write: 16 * 1024,
        }
    }
}

/// Splits large writes into chunks of at most `max` bytes before they reach the TCP stack.
struct CappedWrites<S> {
    inner: S,
    max: usize,
}

impl<S> CappedWrites<S> {
    fn new(inner: S, max: usize) -> Self {
        Self { inner, max }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for CappedWrites<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for CappedWrites<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let n = if self.max == 0 {
            data.len()
        } else {
            data.len().min(self.max)
        };
        std::pin::Pin::new(&mut self.inner).poll_write(cx, &data[..n])
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Starts capturing from `device` (any packet-oriented AsyncRead + AsyncWrite). Must be called
/// inside a Tokio runtime.
pub fn start<D>(device: D, opts: NetstackOptions, handler: TransparentHandler) -> Result<VpnCapture>
where
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut cfg = IpStackConfig::default();
    cfg.mtu(opts.mtu).map_err(|e| anyhow::anyhow!("mtu: {e}"))?;
    cfg.udp_timeout(Duration::from_secs(60));
    let mut stack = IpStack::new(cfg, device);
    let stats = Arc::new(NetstackStats::default());
    let s = stats.clone();

    let task = tokio::spawn(async move {
        loop {
            match stack.accept().await {
                Ok(IpStackStream::Tcp(tcp)) => {
                    s.tcp_connections.fetch_add(1, Ordering::Relaxed);
                    let (src, dst) = (tcp.local_addr(), tcp.peer_addr());
                    handler.handle(CappedWrites::new(tcp, opts.max_write), dst, src);
                }
                Ok(IpStackStream::Udp(udp)) => {
                    let dst = udp.peer_addr();
                    if opts.block_quic && dst.port() == 443 {
                        s.quic_blocked.fetch_add(1, Ordering::Relaxed);
                        drop(udp);
                        continue;
                    }
                    s.udp_flows.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        if let Err(e) = relay_udp(udp, dst).await {
                            tracing::debug!(%dst, error = %e, "udp flow ended");
                        }
                    });
                }
                Ok(_) => {} // ICMP and other protocols are not relayed in the prototype
                Err(e) => {
                    tracing::info!(error = %e, "netstack stopped");
                    break;
                }
            }
        }
    });
    Ok(VpnCapture { task, stats })
}

/// Relays one UDP flow (DNS, NTP, …) through a normal socket owned by the Relay process.
async fn relay_udp(mut flow: ipstack::IpStackUdpStream, dst: SocketAddr) -> io::Result<()> {
    let bind: SocketAddr = if dst.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let sock = UdpSocket::bind(bind).await?;
    sock.connect(dst).await?;
    let mut from_app = vec![0u8; 65535];
    let mut from_net = vec![0u8; 65535];
    loop {
        tokio::select! {
            n = flow.read(&mut from_app) => {
                let n = n?;
                if n == 0 { return Ok(()); }
                sock.send(&from_app[..n]).await?;
            }
            n = sock.recv(&mut from_net) => {
                let n = n?;
                flow.write_all(&from_net[..n]).await?;
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => return Ok(()),
        }
    }
}
