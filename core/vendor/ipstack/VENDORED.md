# Vendored: ipstack 1.0.1 (Apache-2.0)

Source: https://crates.io/crates/ipstack/1.0.1 (unchanged except for the patch below).
Used through `[patch.crates-io]` in `core/Cargo.toml`.

## Relay patch (marked `relay patch` in `src/stream/tcp.rs`)

Problem: when the app side of a VPN-captured TCP connection closed, a reader parked in
`poll_read` was not woken.

- On **RST** the state became `Closed` but the packet task kept waiting for another packet, so
  the reader slept until the ~60 s session timeout.
- On **FIN** the state became `CloseWait` without waking the reader, and `poll_read` only reported
  EOF once the whole session was `Closed`.

Effect in Relay: tunnels (and any app connection) looked open for up to a minute after the app
had left. Found on Android 12 and 17 emulators, 2026-11-21.

Fix:

1. RST wakes the reader and writer and ends the task.
2. FIN wakes the reader.
3. `poll_read` returns EOF in `CloseWait`/`LastAck`/`TimeWait` once buffered data is consumed.

Regression tests: `relay-netstack/tests/close.rs`. To-do: report upstream and drop this copy when
a fixed release ships.
