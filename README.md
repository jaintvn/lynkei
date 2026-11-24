<div align="center">

<img src="docs/logo.png" width="112" alt="Lynkei logo" />

# Lynkei

**See, change and replay every API call your iOS and Android app makes.**<br/>
An open-source, Charles-class HTTP(S) debugging proxy for mobile developers: native Mac app, Rust engine, built-in MCP server for AI agents.

[![Download](https://img.shields.io/badge/Download-v0.2.0--test-6366f1?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/jaintvn/lynkei/releases)
![macOS 14+](https://img.shields.io/badge/macOS-14%2B-111827?style=for-the-badge&logo=apple)
![Apple Silicon + Intel](https://img.shields.io/badge/Apple%20Silicon%20%2B%20Intel-supported-111827?style=for-the-badge)
![Rust](https://img.shields.io/badge/Rust-engine-b7410e?style=for-the-badge&logo=rust&logoColor=white)
![SwiftUI](https://img.shields.io/badge/SwiftUI-Mac%20app-f05138?style=for-the-badge&logo=swift&logoColor=white)
![MCP](https://img.shields.io/badge/MCP-agent%20ready-10b981?style=for-the-badge)

[**Download**](#-download) · [**Features**](#-everything-lynkei-does) · [**Quick start**](#-quick-start) · [**Full guide**](docs/GUIDE.md) · [**Plan**](PLAN.md)

<br/>

<img src="docs/screenshots/traffic.jpg" alt="Lynkei Traffic screen with an iPhone simulator and an Android emulator connected" width="920" />

<sub>Traffic from an iOS Simulator and an Android emulator, captured live. GraphQL operations are split into their own endpoints.</sub>

</div>

---

## ✨ Why Lynkei

|  |  |
|---|---|
| 🔌 **One-click device setup** | Pick a booted iOS Simulator, Android emulator or USB phone. Lynkei sets the proxy *and* installs the certificate, then tells you when traffic arrives. |
| 🔐 **Decrypts HTTPS** | HTTP/1.1, HTTP/2, h2c, WebSockets and TLS interception, with plain-language reasons when an app pins its certificate. |
| 🧰 **Change traffic, don't just watch it** | Breakpoints, mock responses, rewrite rules, DNS spoofing, throttling and more, all from a right-click. |
| 🧠 **Understands your API** | GraphQL operations, gRPC calls (no `.proto` needed) and analytics events (GA4, Segment, Amplitude, Mixpanel, AppsFlyer) are decoded for you. |
| 🤖 **Built for AI agents** | A bundled MCP server lets Claude Code, Cursor or any MCP client connect a device, read traffic and add rules. |
| 🪶 **Native and fast** | A Rust engine under a SwiftUI app. Crash-safe: your Mac's proxy settings are restored even if Lynkei dies. |

---

## 📥 Download

Grab the build for your Mac from the [latest release](https://github.com/jaintvn/lynkei/releases):

| Mac | File |
|---|---|
| Apple Silicon (M1 and later) | `Lynkei-mac-apple-silicon.zip` |
| Intel | `Lynkei-mac-intel.zip` |
| Android demo app | `AcmeShop-demo.apk` |

> [!NOTE]
> The builds are not notarized yet. If macOS blocks the app, unzip it and run `xattr -dr com.apple.quarantine Lynkei.app`, or use *System Settings → Privacy & Security → Open Anyway*. Downloading with `gh release download` or `curl` avoids the block.

---

## 🚀 Quick start

1. **Open Lynkei.** It starts a proxy on `127.0.0.1:8888` and creates its own certificate authority.
2. Click **Connect device**. Booted simulators and emulators show up as cards. Press **Connect**.
3. Use your app. Calls appear live, with headers, bodies, timing and the rules that touched them.

<div align="center">
<img src="docs/screenshots/connect.jpg" alt="Connect a device: iOS Simulator and Android emulator with proxy, certificate and traffic checks" width="860" />
<br/><sub>Each device card ticks off <b>Proxy → Certificate trusted → Traffic seen</b>, and explains anything it can't do (here, an emulator without root).</sub>
</div>

---

## 🔭 Everything Lynkei does

### 📡 Capture and inspect

<div align="center">
<img src="docs/screenshots/inspector.jpg" alt="Inspector showing a decrypted JSON response" width="860" />
</div>

- **Live traffic list** with status, method, type badge, host, device, duration bar and size. Filter by status class, method, content type, device or API version, or group calls by endpoint.
- **Resizable panels.** Drag the sidebar and inspector edges to give long endpoints room; the table adapts its columns as you resize.
- **Inspector** with *Overview, Request, Response, Timing* and *Logs* tabs, a full-size body viewer (pretty JSON, search, edit, field view) and one-click **Copy cURL**.
- **Search language:** `status:5xx host:api path:/v2/* duration:>800ms size:>1MB body:"out of stock" header:x-app-version=3.4 state:failed app:AcmeShop`. Bare words match URLs, headers and bodies. Prefix `-` to negate.
- **Searchable history** stored in SQLite with full-text search, so yesterday's session is still there.
- **App and device attribution:** every call shows the program or device that made it.
- **Favourites** for the endpoints you watch most.
- **HAR import and export** (HAR 1.2), compatible with browsers and Charles.
- **Compose and Repeat:** write a request or resend a captured one, edited, up to 200 times, one by one or all at once. Great for retry and race checks.

### 📊 Dashboard and endpoints

<div align="center">
<img src="docs/screenshots/dashboard.jpg" alt="Dashboard with requests per second, error rate, p95 latency and failing endpoints" width="860" />
</div>

- **Dashboard:** requests per second, error rate, p95 latency, data transferred, failing and slowest endpoints, top hosts and a live feed. Scope it to one device or app.
- **Endpoints view:** IDs fold automatically (`/users/8231` → `/users/{id}`). Sort by *most failing, slowest, most called* or *largest*, with p50, p95, average size and a 15-minute sparkline.

<div align="center">
<img src="docs/screenshots/endpoints.jpg" alt="Endpoints view with error rate and latency per endpoint" width="860" />
</div>

### 🧬 Understands your protocols

<div align="center">
<img src="docs/screenshots/graphql.jpg" alt="GraphQL tab showing the query and variables of a GetProducts operation" width="860" />
</div>

- **GraphQL:** every operation becomes its own endpoint (`POST /graphql · GetProducts`), batches are split, persisted queries are recognised, and a *GraphQL* tab shows the query and variables.
- **gRPC:** calls show as `Service/Method` with their status. Messages are decoded **without a `.proto` file**, as JSON keyed by field number.
- **Analytics events:** calls to Google Analytics 4, Segment, Amplitude, Mixpanel and AppsFlyer get an *Events* tab with event names, user and properties.
- **WebSockets:** frames are recorded in both directions.
- **Device logs:** with a simulator or Android device attached, the *Logs* tab shows what the app logged around the call (`simctl log stream` and `adb logcat`).

### 🛠 Change traffic

<div align="center">
<img src="docs/screenshots/rules.jpg" alt="Rule builder with Modify, Mock, Redirect, Pause, Block and Slow network" width="860" />
</div>

Right-click any call to add a rule, or build one in **Rules**. Everything is a plain `rules.yaml`, so it also works from the CLI and API.

| Rule | What it does |
|---|---|
| ⏸ **Breakpoints** | Hold a matching request and/or response, edit status, headers or body, then *Resume* or *Abort*. A forgotten one is released after 5 minutes. |
| 🎭 **Map local (Mock)** | Answer from a file, or reuse a recorded response. No server needed. |
| 🔀 **Map remote (Redirect)** | Send a host to another, such as staging. |
| ✏️ **Rewrite** | Set or remove headers, edit JSON fields, replace text (plain or regex) or the whole body, change the status, delay the reply. Point-and-click, like Charles' Rewrite. |
| 🌐 **DNS spoofing** | Connect to another address while the app still sees the original host, so certificates and `Host` headers stay intact. |
| 🐢 **Throttle** | 3G, Edge, LTE, lossy LTE, high latency or offline, per rule or globally. |
| ⛔ **Block** | Fail any host or path. |

Each request row shows a badge per rule that touched it, and the inspector lists every rule that fits that endpoint with its on/off switch. A **live preview** shows how many recent requests a rule would match and a before/after diff.

### 🎬 Scenarios

Record a flow once (*Rules → Scenarios → Record*), then **replay it offline** for demos and UI tests. Replays answer from the recording as mock rules, with no server. The same files are used by the MCP server.

### 🤝 Share a session

*File → Share Session…* (⇧⌘S) saves the calls in the list, and optionally your rules, scenarios and nearby logs, into one `.lynkei` file. Authorization, Cookie and API-key headers are hidden by default. A teammate double-clicks the file and sees everything.

### 🏝 The notch island

A small pill hugging the notch (a floating pill on Macs without one) shows request rate, held breakpoints and new failures. Expand it to see the latest calls, connect or disconnect devices, toggle **SSL decrypt**, pick a **throttle** preset or flip whole rule kinds on and off. When a breakpoint holds a call, it opens by itself with **Continue**, **Edit…** and **Abort**. It also warns about **certificate pinning**, **failure spikes** and **disconnected devices**.

### 🔒 Certificates, made painless

- **Install on this Mac** from the Connect screen (adds Lynkei's root to your login keychain), or *Help → Install Lynkei Certificate on This Mac…*.
- **Android over Wi-Fi:** scan a QR code, which opens a page with the certificate and the exact proxy settings. *No cable? Pair with adb* uses Wireless debugging.
- **iPhone over Wi-Fi:** scan a QR code. Wired iPhones are listed too.
- **Plain-language TLS help:** when an app rejects Lynkei's certificate (pinning, or a user CA it ignores), the row says so and the banner explains what to do. Apps that can't be decrypted are tunnelled untouched, so they keep working.
- **Live Mac proxy status:** warns when a VPN or company profile switches your Mac's proxy off, with a button to turn it back on.

---

## 🤖 AI agents (MCP)

`lynkei-mcp` is a Model Context Protocol server over the same control API the app and CLI use. An agent can connect a device, search traffic, add or toggle rules, send and repeat requests, handle paused calls and record scenarios.

```bash
cd core && cargo build -p lynkei-mcp --release
claude mcp add lynkei -- /path/to/lynkei-mcp      # Claude Code
```

For any other MCP client: `{ "mcpServers": { "lynkei": { "command": "/path/to/lynkei-mcp" } } }`.

**28 tools** (`search_traffic`, `get_transaction`, `add_rule`, `repeat_request`, `record_scenario`, `wait_paused`, …), live **resources** (`lynkei://traffic?query=status:5xx`), and ready-made **prompts** (`debug_failing_endpoint`, `mock_api_offline`, `find_slow_calls`). Secrets are redacted and bodies left out unless the call asks for them.

---

## ⌨️ Command line and daemon

```bash
./target/release/lynkeid                       # proxy on 127.0.0.1:8888 + control API
./target/release/lynkei status
./target/release/lynkei search status:5xx host:api.acme.com 'duration:>800ms'
./target/release/lynkei tail -state:tunneled   # live events
./target/release/lynkei devices connect <id>
./target/release/lynkei throttle 3g
./target/release/lynkei breakpoints wait       # block until a call is held
./target/release/lynkei system-proxy on        # restored on exit or crash
```

---

## 🏗 Architecture

```
core/                    Rust workspace, no platform code
  lynkei-proxy/            Proxy engine: TLS MITM, HTTP/1.1 + HTTP/2, WebSocket, streaming capture
  lynkei-model/            Data model, search language, rules format
  lynkei-store/            SQLite + FTS5 traffic history
  lynkei-control/          Control API (JSON-RPC over a Unix socket), system proxy + crash guard
  lynkei-devices/          Find and connect iOS Simulators, Android emulators and USB phones
  lynkei-ffi/              UniFFI boundary to Swift
  lynkei-mcp/              MCP server for AI agents
  lynkeid/, lynkei-cli/    Daemon and command line
apple/Lynkei/            SwiftUI Mac app
ios/AcmeShop/            Demo iOS app that calls a local mock API
```

A Lynkei app for Android and an iOS companion are planned once there is user feedback.

---

## 🧪 Build from source

Needs Xcode 16+ and Rust via rustup.

```bash
scripts/build-apple.sh && scripts/make-mac-app.sh && open apple/Lynkei/build/Lynkei.app
```

```bash
scripts/make-mac-app.sh release arm64      # Apple Silicon only
scripts/make-mac-app.sh release x86_64     # Intel only (run UNIVERSAL=1 scripts/build-apple.sh first)
scripts/check.sh                           # fmt, clippy, tests and the Apple build, locally
```

**Try it with the demo app:** `scripts/demo-server.sh` starts a local mock API; connect a simulator in Lynkei, open **Acme Shop** and tap **Run every call**. Details are in the [full guide](docs/GUIDE.md#try-it-with-the-demo-app).

---

## 📚 Learn more

- **[Full guide](docs/GUIDE.md):** rule syntax, breakpoints from a terminal, rewrite sheet, search language, MCP reference, demo apps.
- **[PLAN.md](PLAN.md):** roadmap and status.

<div align="center"><sub>Made for people who are tired of guessing what their app sends.</sub></div>
