# Relay

An open-source, Charles-class HTTP(S) debugging proxy for iOS and Android developers. A shared Rust engine (`core/`) is used by a SwiftUI Mac app and a Kotlin Android app (on-device capture through a VPN). See [PLAN.md](PLAN.md) for the full plan and the [UI design canvas](https://claude.ai/artifact/8Av8hoMna1fDGx5AWLe5SE).

**Status: P1 complete.** Every feature below is in place and has been checked on macOS and on Android emulators (12, 15 and 17):

- HTTP/1.1, HTTP/2 and h2c, decrypted HTTPS and WebSockets.
- Streaming capture.
- Searchable history.
- A rules format.
- A daemon with a control API and CLI.
- Crash-safe system proxy.

## Layout

```
core/                 Rust workspace (no platform code)
  relay-model/        Data model, search language (filter.rs), rules format (rules.rs)
  relay-proxy/        Proxy engine: listener, TLS MITM, HTTP/1.1 + HTTP/2, WebSocket, streaming capture
  relay-netstack/     VPN packets → TCP/UDP streams (Android); uses vendor/ipstack (patched)
  relay-store/        SQLite + FTS5 traffic history
  relay-control/      Control API (JSON-RPC over a Unix socket), engine, system proxy + guard
  relay-devices/      Find and connect iOS Simulators, Android emulators and USB phones
  relayd/             The engine daemon
  relay-ffi/          UniFFI boundary → Swift and Kotlin
  relay-cli/          `relay` command-line tool
  relay-bench/        Benchmarks against local origins
  vendor/ipstack/     ipstack 1.0.1 with a close-handling fix (see VENDORED.md)
apple/RelayKit/       Swift package wrapping the generated bindings + XCFramework
apple/Relay/          SwiftUI Mac app
android/relay-android Kotlin app: VpnService capture, foreground service
shared/rules-schema/  JSON Schema + example for the rules YAML
scripts/              build-apple.sh, make-mac-app.sh, build-android.sh, test-android.sh
```

## Prerequisites

- Xcode 16+ (Swift 6), Rust via rustup (`brew install rustup && rustup default stable`)
- Android: SDK + NDK, `cargo install cargo-ndk`, `rustup target add aarch64-linux-android`

Homebrew's rustup is keg-only, so put it on your PATH:

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
```

## Run the daemon and CLI

```bash
cd core && cargo build --release -p relayd -p relay-cli
```

```bash
./target/release/relayd          # proxy on 127.0.0.1:8888 + control API
```

In another terminal:

```bash
./target/release/relay status
```

```bash
./target/release/relay search status:5xx host:api.acmeshop.com 'duration:>800ms'
```

```bash
./target/release/relay tail -state:tunneled          # live events
```

```bash
./target/release/relay rules check ../shared/rules-schema/example.rules.yaml
```

```bash
./target/release/relay system-proxy on       # points this Mac at Relay; restored on exit or crash
```

`relayd launch-agent --write` installs a launchd agent that keeps it running.

### Search language

```
status:5xx  status:>=400  method:POST  host:acmeshop.com  path:/v2/*  duration:>800ms  size:>1MB
body:"out of stock"  header:x-app-version=3.4  state:failed|tunneled|open  decrypted:true  version:h2
```

Bare words match the URL, headers and bodies. Put `-` in front of a term to negate it, and quote values that contain spaces.

## Mac app

```bash
scripts/build-apple.sh && scripts/make-mac-app.sh && open apple/Relay/build/Relay.app
```

Click **Connect device**. Relay finds booted iOS Simulators and Android emulators or USB phones and sets each one up (proxy and certificate) with one click. For an iPhone on Wi-Fi, scan the QR code on that screen. From a terminal, `relay devices connect <id>` does the same.

## Android app (on-device capture)

```bash
scripts/build-android.sh && (cd android/relay-android && ./gradlew assembleDebug)
```

Install the APK, enter the package to capture, and tap **Start capture**. Capture runs as a foreground service with a notification that has a Stop action.

## Tests and benchmarks

```bash
cd core && cargo test --workspace
```

```bash
scripts/test-android.sh
```

This runs the core suites on a connected arm64 emulator.

```bash
cd core && cargo run -p relay-bench --release -- --store
```

This measures added latency, request rate and throughput.

CI (`.github/workflows/ci.yml`) runs fmt, clippy, tests, the Apple build and the Android build.
