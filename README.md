# Relay

An open-source, Charles-class HTTP(S) debugging proxy for iOS and Android developers. A shared Rust engine (`core/`) is used by a SwiftUI Mac app now and a Kotlin Android app later. See [PLAN.md](PLAN.md) for the full plan and the [UI design canvas](https://claude.ai/artifact/8Av8hoMna1fDGx5AWLe5SE).

**Status: P0 spike.** HTTP/1.1, CONNECT and TLS interception work end to end on macOS and Android.

## Layout

```
core/                 Rust workspace (no platform code)
  relay-model/        Transaction data model
  relay-proxy/        Proxy engine: listener, TLS MITM, local CA, capture
  relay-ffi/          UniFFI boundary → Swift (and Kotlin later)
  relay-cli/          `relay` command-line tool (also runs on Android)
apple/RelayKit/       Swift package wrapping the generated bindings + XCFramework
apple/Relay/          SwiftUI Mac app (P0)
scripts/              build-apple.sh, make-mac-app.sh
design/               Shared UI brief
```

## Prerequisites

- Xcode 16+ (Swift 6), Rust via rustup (`brew install rustup && rustup default stable`)
- Android: Android SDK + NDK, `cargo install cargo-ndk`, `rustup target add aarch64-linux-android`

Homebrew's rustup is keg-only, so put it on your PATH:

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
```

## Build and run

```bash
cd core && cargo test                  # unit + offline end-to-end TLS tests
cargo run -p relay-cli -- serve        # proxy on 127.0.0.1:8888, prints each request
```

Mac app:

```bash
scripts/build-apple.sh                 # Rust → XCFramework + Swift bindings
scripts/make-mac-app.sh                # → apple/Relay/build/Relay.app
open apple/Relay/build/Relay.app
```

Try it:

```bash
curl --proxy http://127.0.0.1:8888 --cacert "$HOME/Library/Application Support/Relay/relay-ca.pem" https://httpbin.org/json
```

- **iOS Simulator:** in the app, choose **Simulators → Trust Relay CA**, then set this Mac's HTTP and HTTPS proxy to `127.0.0.1:8888`. A privileged helper will do this automatically in P1.
- **Android emulator or USB device** (no Wi-Fi needed): run `adb reverse tcp:8888 tcp:8888`. Then point the device's proxy at `127.0.0.1:8888` with `adb shell settings put global http_proxy 127.0.0.1:8888`, and undo it afterwards with `adb shell settings put global http_proxy :0`.
- **Engine on Android:**

  ```bash
  ANDROID_NDK_HOME=… cargo ndk -t arm64-v8a --platform 26 build -p relay-cli --release
  ```

  Then `adb push` the binary and run `relay serve` on the device.

Any device can download the CA by opening `http://relay.ca` while it uses the proxy.
