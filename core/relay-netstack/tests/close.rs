//! Regression tests for app-side connection close through the user-space TCP stack.
//!
//! A fake TUN device (in-memory pipe) carries hand-built IPv4/TCP packets from an "app" into
//! ipstack. When the app resets or finishes the connection, a reader on the accepted stream must
//! wake up promptly. Before the vendored patch it slept until ipstack's ~60 s session timeout,
//! which kept Relay tunnels "open" long after the app had gone (see vendor/ipstack/VENDORED.md).

use std::time::Duration;

use etherparse::{PacketBuilder, SlicedPacket, TransportSlice};
use ipstack::{IpStack, IpStackConfig, IpStackStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const APP: [u8; 4] = [10, 111, 0, 2];
const SERVER: [u8; 4] = [17, 253, 144, 10];
const APP_PORT: u16 = 40_000;
const SERVER_PORT: u16 = 443;
const APP_ISN: u32 = 1_000;

/// Our end of the fake TUN: writes app packets in, reads the stack's packets out.
struct Tun {
    dev: DuplexStream,
    pending: Vec<u8>,
}

impl Tun {
    async fn send(&mut self, packet: Vec<u8>) {
        self.dev.write_all(&packet).await.unwrap();
        // One packet per device read: give the stack time to take it before the next write.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    /// Next whole IPv4 packet from the stack (several may arrive in one read).
    async fn recv(&mut self) -> Vec<u8> {
        loop {
            if self.pending.len() >= 4 {
                let len = u16::from_be_bytes([self.pending[2], self.pending[3]]) as usize;
                if self.pending.len() >= len {
                    return self.pending.drain(..len).collect();
                }
            }
            let mut buf = [0u8; 4096];
            let n = self.dev.read(&mut buf).await.unwrap();
            assert!(n > 0, "stack closed the device");
            self.pending.extend_from_slice(&buf[..n]);
        }
    }
}

fn tcp(seq: u32, ack: Option<u32>, flags: &str) -> Vec<u8> {
    let mut b = PacketBuilder::ipv4(APP, SERVER, 64).tcp(APP_PORT, SERVER_PORT, seq, 65_535);
    if flags.contains('S') {
        b = b.syn();
    }
    if flags.contains('F') {
        b = b.fin();
    }
    if flags.contains('R') {
        b = b.rst();
    }
    if let Some(a) = ack {
        b = b.ack(a);
    }
    let mut out = Vec::new();
    b.write(&mut out, &[]).unwrap();
    out
}

/// Completes a handshake and returns (tun, accepted stream, the stack's next sequence number).
async fn established() -> (Tun, ipstack::IpStackTcpStream, u32, IpStack) {
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let mut stack = IpStack::new(IpStackConfig::default(), theirs);
    let mut tun = Tun {
        dev: ours,
        pending: Vec::new(),
    };

    tun.send(tcp(APP_ISN, None, "S")).await;
    let stream = match tokio::time::timeout(Duration::from_secs(2), stack.accept())
        .await
        .unwrap()
        .unwrap()
    {
        IpStackStream::Tcp(t) => t,
        _ => panic!("expected a TCP stream"),
    };
    let syn_ack = tun.recv().await;
    let parsed = SlicedPacket::from_ip(&syn_ack).unwrap();
    let Some(TransportSlice::Tcp(t)) = parsed.transport else {
        panic!("expected TCP")
    };
    assert!(t.syn() && t.ack(), "SYN-ACK from the stack");
    let server_next = t.sequence_number().wrapping_add(1);
    tun.send(tcp(APP_ISN + 1, Some(server_next), "A")).await;
    (tun, stream, server_next, stack)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_wakes_when_app_resets() {
    let (mut tun, mut stream, server_next, _stack) = established().await;
    let reader = tokio::spawn(async move {
        let mut buf = [0u8; 64];
        stream.read(&mut buf).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await; // reader is parked

    tun.send(tcp(APP_ISN + 1, Some(server_next), "RA")).await;
    let result = tokio::time::timeout(Duration::from_secs(1), reader).await;
    assert!(
        result.is_ok(),
        "reader must wake within 1 s after RST (was ~60 s before the patch)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_sees_eof_when_app_finishes() {
    let (mut tun, mut stream, server_next, _stack) = established().await;
    let reader = tokio::spawn(async move {
        let mut buf = [0u8; 64];
        stream.read(&mut buf).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await; // reader is parked

    tun.send(tcp(APP_ISN + 1, Some(server_next), "FA")).await;
    let n = tokio::time::timeout(Duration::from_secs(1), reader)
        .await
        .expect("reader must wake within 1 s after FIN")
        .unwrap()
        .expect("clean EOF, not an error");
    assert_eq!(n, 0, "FIN means end-of-stream");
}
