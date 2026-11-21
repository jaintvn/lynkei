# Relay — an open-source Charles Proxy alternative for macOS

> Working name: **Relay**. You can rename it.
> Goal: Charles-level power (capture, SSL, breakpoints, rewrite, map local/remote, throttle), with **one-click device setup**, a modern UI, a notch "Dynamic Island", and **full MCP control** so AI agents can drive every operation.
> Products: **Relay for Mac** first, then **Relay for Android**, an on-device debugger that captures and edits traffic of apps on the phone itself, with no Mac needed. Both share one engine, one rule format and one session format, so the Android app is mostly UI work when it starts.

---

## 0. What Charles 5.2.2 ships (reference inventory)

I looked at the bundle you downloaded (`charles-proxy-5.2.2.dmg`) only to build a feature list. No code or assets get copied. Relay is a clean-room build.

- **Stack:** Java 25 (bundled JDK), Swing + VAqua look-and-feel, BouncyCastle for certificates, hpack for HTTP/2, protobuf, jfreechart, a small JNI library for macOS hooks.
- **Proxy engine (`com.xk72.proxy`):** HTTP/1.1, HTTP/2 (`Http2Endpoint`), HTTP/3 header model (`Http3Fields`), WebSocket, SOCKS (`SOCKSProxyServer`), transparent proxy, reverse proxy, UDP forwarding (`DatagramForwardingProxyServer`), and a TLS ClientHello/record snooper (`SSLSnooper`, `SSLHandshakeParser`).
- **Tools (`com.charlesproxy.tools`):** Breakpoints, Rewrite (rule sets), Map Local, Map Remote (`MapTool`), Compose / Compose New / Repeat, Mirror, Auto Save, Block List, Allow List, Block Cookies, No Caching, DNS Spoofing, Port Forwarding, Reverse Proxies, Client Process (which Mac process made the request), Throttle (`LatencyOutputStream`).
- **Viewers:** JSON, XML, protobuf (with descriptor import), AMF, WebSocket messages, images, a request diff, timing charts, Structure/Sequence views, Find, and a validator.
- **Other:** SSL client certificates, HAR and other import/export, configuration import/export, and a remote-control "command" interface.

**Parity target:** everything above except AMF (Flash, obsolete) and the validator (low value).

---

## 1. Architecture: one shared core, thin native shells

Because an Android app is coming, the engine **must not be Mac-only**. Everything that isn't UI or OS glue lives in a portable **Rust core** (`relay-core`). The Mac app (Swift) and the Android app (Kotlin) are thin native shells around it.

```
                 ┌──────────────────────── relay-core (Rust) ────────────────────────┐
                 │ Listeners: HTTP · CONNECT · SOCKS5 · transparent · TUN packet in  │
                 │ TLS MITM (rustls + rcgen) · HTTP/1 + HTTP/2 (hyper/h2) · WebSocket│
                 │ Rule engine (breakpoints, rewrite, map local/remote, block,       │
                 │   throttle, chaos) · Filter DSL · endpoint templating             │
                 │ Store: SQLite (rusqlite, WAL, FTS5) + zstd body blobs             │
                 │ Control API (JSON-RPC + event stream) · MCP server (rmcp)         │
                 │ Session (.relay) + rules (YAML) formats, versioned schemas        │
                 └────────▲────────────────────────────────────────────▲─────────────┘
               UniFFI (Swift bindings)                        UniFFI (Kotlin bindings / JNI)
┌──────────────────────────┴──────────────────┐   ┌─────────────────────┴────────────────────┐
│ macOS                                       │   │ Android (later)                          │
│ relayd daemon (launchd) hosts relay-core    │   │ VpnService foreground service hosts      │
│ Relay.app: SwiftUI + AppKit, notch Island,  │   │ relay-core; TUN → userspace TCP/IP       │
│   menu bar extra                            │   │ (smoltcp) → same engine                  │
│ relay-helper: system proxy + pf, dead-man   │   │ Compose UI, notification "island",       │
│ Device mgr: simctl, adb, QR/PAC, mDNS       │   │   Quick Settings tiles, bubble overlay   │
│ relay CLI · relay-mcp (stdio shim)          │   │ Pair-with-Mac mode (tunnel to relayd)    │
└─────────────────────────────────────────────┘   └──────────────────────────────────────────┘
```

### Why it's split this way
- **Portable core.** The proxy, TLS, rules, store, filter DSL, control API and MCP are written once and tested once. On Android the core runs on-device; on the Mac it runs in `relayd`. Rule files and session files move between the two unchanged.
- **Engine in a separate process on the Mac.** A UI crash or hang never drops device connections, and the MCP server, CLI and UI all share one source of truth.
- **Privileged helper does very little.** It only sets and restores the system proxy and loads pf rules. It also has a **dead-man switch**: if `relayd` stops heartbeating, the helper restores the original network settings. This fixes Charles's classic "no internet after a crash" problem.
- **One control API on every platform.** Every UI action is an API call, so MCP coverage is complete by design. The same API lets the Mac app view and control the Android app's engine (over `adb forward` or LAN), and lets the Android app act as a client of the Mac.

### Why Rust, not Swift, for the core (decided)
| | Rust core (recommended) | Swift core (SwiftNIO) |
|---|---|---|
| Android support | First-class (NDK targets, `cargo-ndk`, UniFFI → Kotlin) | Swift-on-Android SDK exists but is young; little production use for networking |
| On-device VPN capture | `smoltcp` userspace TCP/IP stack is mature | No equivalent; you'd write your own |
| Libraries | tokio, hyper/h2, rustls, rcgen, rusqlite, zstd, `rmcp` (official MCP SDK) | SwiftNIO stack is excellent, but on Apple platforms only |
| Mac UI integration | UniFFI-generated Swift bindings + async callbacks | Native |
| Cost | One FFI boundary to design carefully | Engine rewrite later for Android |

Choosing Swift now would mean writing the engine a second time for Android. Rust adds one FFI boundary but keeps a single engine.

### Tech stack
| Area | Choice | Reason |
|---|---|---|
| Core | **Rust** (tokio, hyper 1.x + h2, rustls, rcgen, tokio-tungstenite, rusqlite + FTS5, zstd, smoltcp, rmcp) | Fast, memory-safe, runs on macOS and Android |
| Bindings | **UniFFI** → Swift package (XCFramework) and Kotlin AAR | One interface definition, generated bindings for both apps |
| Certificates | rcgen leaf certs (P-256) signed by a local CA (RSA-2048 for old Android compatibility) | No OpenSSL shelling |
| Storage | SQLite (WAL) metadata + FTS5 search; bodies as zstd blobs on disk; **identical schema on both platforms** | 1M+ rows on Mac, quota-limited on phone |
| Mac UI | Swift 6, SwiftUI shell; **NSTableView** (wrapped) for the traffic list; TextKit 2 viewers | SwiftUI `Table` struggles past about 50k rows |
| Android UI (later) | Kotlin, Jetpack Compose, Material 3, Paging 3 over the core's cursor API | Smooth lists backed by the same store |
| MCP | `rmcp` inside the core; `relay-mcp` is a tiny stdio shim that connects to whichever engine you target | Same tools on Mac and Android |
| Shared design | Design tokens (colors, type, spacing, status colors) in one JSON file → generated Swift and Compose themes | The two apps feel like one product |
| Distribution | Mac: Developer ID–signed, notarized DMG, Sparkle, Homebrew cask. Android: GitHub Releases APK + F-Droid (Play Store is restrictive about VPN/MITM apps) | Open source, MIT or Apache-2.0 |
| Min OS | macOS 14 · Android 8.0 (API 26) | SMAppService/Observation · `getConnectionOwnerUid` needs API 29 for exact attribution, with a fallback below that |

### Repo layout (monorepo)
```
Relay/
  core/                     # Rust workspace, no UI and no platform-specific code
    relay-model/            # transactions, rules, filter DSL, path templating
    relay-proxy/            # listeners, TLS MITM, h1/h2/ws, rule execution
    relay-netstack/         # TUN packets ↔ TCP/UDP streams (smoltcp); used by Android and the iOS companion
    relay-store/            # SQLite + blob store, session import/export (HAR, .relay, .chlsj)
    relay-control/          # JSON-RPC API + event stream (versioned schema)
    relay-mcp/              # MCP tools/resources mapped onto relay-control
    relay-ffi/              # UniFFI interface → Swift + Kotlin bindings
    relay-cli/
  apple/
    RelayKit/               # Swift package wrapping the generated bindings
    Relay/                  # macOS app (main window, island, menu bar)
    relayd/                 # launchd agent hosting the core
    relay-helper/           # privileged helper
    RelayDevices/           # simctl/adb/usbmux adapters, cert installers, PAC/QR server
    ios-companion/          # (later) optional Network Extension app for physical iPhones
  android/
    relay-android/          # (later) on-device debugger app: VpnService + Compose UI
    relay-debug-lib/        # Gradle plugin/AAR for target apps: trust user CA in debug builds
  shared/
    design-tokens/          # tokens.json → Swift + Compose themes
    rules-schema/           # JSON Schema for rule YAML (validated on both platforms)
    fixtures/               # sample sessions and rules used by tests on both platforms
  tests/  bench/  docs/
```

### Rules that keep the Android app cheap later
1. **No platform code in `core/`.** Anything OS-specific (system proxy, simctl, adb, VpnService) goes behind a trait that the shell implements.
2. **Everything the UI shows comes from relay-control.** No Mac-only shortcuts into the store. The Android UI then needs zero engine work.
3. **Versioned formats from day one:** rules YAML, `.relay` sessions and the control API each carry a schema version, with forward-compatible parsing.
4. **The core must run with tight memory and battery budgets**, not just on a Mac: configurable caps, no busy loops, idle wakeups under 1/s.
5. **CI builds the core for `aarch64-linux-android` from P0**, with tests on an Android emulator, even before any Android UI exists.

---

## 2. Proxy engine (connectivity is the priority)

### Pipeline per connection (tokio, in relay-core)
`Ingress (TCP listener on Mac · TUN stream on Android) → sniff first bytes (HTTP / CONNECT / SOCKS5 / TLS ClientHello) → [CONNECT → TLS decision] → HTTP/1 or HTTP/2 codec → RuleChain → upstream connection pool → response RuleChain → client`

The only difference between platforms is the ingress. On the Mac, connections arrive at a proxy listener. On Android, `relay-netstack` turns VPN packets into the same TCP streams, and from there the pipeline is identical. Connections from the TUN are treated like transparent-mode traffic: the destination comes from the packet, and the host from SNI or the `Host` header.

- **Streaming by default.** Bodies stream straight through, and a tee writes them to the store asynchronously. Bodies are buffered only when a rule needs the full body (breakpoint, body rewrite). SSE, gRPC streaming, long-poll and large downloads never stall.
- **Protocols:** HTTP/1.1 (keep-alive, chunked, `Expect: 100-continue`), HTTP/2 (both legs, ALPN-negotiated per side, so h2 client → h1 server works), WebSocket (frame-level capture and editing), gRPC (h2 trailers), SOCKS5, transparent mode (pf `rdr` for apps that ignore the proxy).
- **HTTP/3/QUIC:** with a proxy configured, clients fall back to TCP. In transparent mode, block UDP/443 to force the fallback, and say so in the UI.
- **TLS:**
  - Per-host leaf certs signed by a locally generated CA, kept in an LRU cache and pre-warmed for recent hosts.
  - SNI is preferred over the CONNECT host.
  - The upstream TLS mirrors the client's ALPN.
  - Supports a custom upstream CA, client certificates (mTLS), and an "ignore upstream cert errors" toggle per host.
- **Pinning detection.** If the client aborts a handshake after receiving our cert (alert 42/46/48, or an immediate close), Relay marks the host "pinned?". It then offers a one-click passthrough rule, or auto-adds one if the user opted in, so a pinned host never breaks the app.
- **Default bypass list** (passthrough, never decrypted): Apple push/iCloud/App Store, Google Play services/FCM, OS update hosts. The user can edit it. This prevents broken push and sign-in.

### Robustness checklist (each gets an automated test)
- [ ] System proxy is restored on quit, crash, `kill -9`, sleep/wake and logout (helper dead-man switch plus launchd `KeepAlive`).
- [ ] Network changes (Wi-Fi switch, VPN up/down, IP change) are handled via `NWPathMonitor`: rebind listeners, regenerate PAC/QR, refresh device configs and notify connected devices.
- [ ] Upstream proxy chaining and corporate PAC files are honored (read via `SCDynamicStore`), with per-host overrides.
- [ ] Port conflicts: try the preferred port, then the next free one, and propagate the change to every device automatically.
- [ ] Timeouts: separate connect, TLS, idle and total timeouts; upstream failures produce a clear synthetic 502 with the reason in the UI.
- [ ] IPv6, DNS-over-HTTPS bypass, and Happy Eyeballs (RFC 8305 connect racing).
- [ ] Backpressure: the client's read rate is honored end to end, so a slow device can't push memory past its cap.
- [ ] Memory caps: body storage quota with a ring-buffer eviction policy; bodies over a limit (default 50 MB) are truncated in the store but always streamed in full.
- [ ] Fuzzing of the HTTP/1 parser, HPACK, the TLS ClientHello sniffer and the TUN packet parser (`cargo fuzz`).
- [ ] FFI boundary: no panics cross UniFFI (catch and convert to errors); callbacks into Swift and Kotlin never block engine threads.

### Performance targets
| Metric | Target |
|---|---|
| Added latency p50 / p99 (local, keep-alive) | < 1 ms / < 5 ms |
| Throughput | ≥ 5k req/s, ≥ 1 Gbps bulk on an M-series Mac |
| Idle memory (engine) | < 60 MB |
| Traffic list | 1M rows, 120 fps scroll, filter < 50 ms (FTS5 + in-memory index) |
| Cold start to proxy ready | < 300 ms |

---

## 3. Device connection (the easy setup)

The **Connect Device** sheet auto-detects targets and shows one big button per device. Each card shows a status checklist: *Proxy ✓ · CA trusted ✓ · Traffic seen ✓*.

| Target | How Relay connects | User effort |
|---|---|---|
| **iOS Simulator** | Detect booted sims (`xcrun simctl list -j`). Install the CA with `simctl keychain <udid> add-root-cert`. Sims use the Mac's network, so route via the system proxy, or with "Simulator-only" mode, a PAC that only the sim's processes hit. Attribute traffic to the app by PID → bundle ID. | 1 click |
| **Android Emulator** | `adb devices` detection. `adb reverse tcp:P tcp:P` + `settings put global http_proxy 127.0.0.1:P`. CA: for user-debug/Google APIs images, `adb root` → install into the system store (Android 14+ APEX/Conscrypt via a mount-namespace bind mount); otherwise install as a user CA and provide the debug-lib config. Clean undo on disconnect. | 1 click |
| **Android device (USB)** | Same `adb reverse` path, so **no shared Wi-Fi is needed** and the IP never changes. Works over wireless ADB too (pair via QR, `adb pair`). | 1 click + "allow USB debugging" |
| **Android device with Relay for Android installed (later)** | **Pair-with-Mac mode:** the phone app's VPN captures every app's traffic, including apps that ignore proxy settings, and forwards it to the Mac's `relayd` over `adb reverse` or LAN. The Mac shows exact per-app attribution (UID → package) from the phone. Pairing is by QR code. | Scan QR once |
| **iPhone/iPad (Wi-Fi)** | The device scans a **QR code**, which opens `http://<mac>.local:P/setup`. A guided page installs a `.mobileconfig` with the CA, then shows a two-step trust guide. Proxy mode: **Automatic → PAC URL** using the mDNS hostname, so it survives Mac IP changes. Relay detects the first request and turns the card green. | Scan QR + 2 toggles |
| **iPhone (later, optional)** | A companion app with `NEPacketTunnelProvider` (reusing `relay-netstack` from the core) that routes to the Mac over USB (usbmux) or Wi-Fi. No proxy settings, and it covers apps that ignore proxies. | Install app once |
| **This Mac / browsers** | System proxy toggle, or per-app via launch with `HTTPS_PROXY`. Firefox and Node/Java get CA export helpers. | 1 click |

**Android user CA trust:** apps targeting API 24+ ignore user CAs. The `android-debug-lib` Gradle plugin injects a debug-only `network_security_config` that trusts user CAs. The setup screen shows a copy-paste XML snippet as an alternative.

**Undo everything:** "Disconnect" reverses each change (proxy setting, `adb reverse`, optionally the cert). A launch-time sweep fixes devices left in a proxied state.

---

## 4. UI design (modern, minimal)

> **Decided (2026-11-20): build from the Claude Design canvas → [Relay UI — Claude Design](https://claude.ai/artifact/8Av8hoMna1fDGx5AWLe5SE).** Look: "Graphite + Electric Indigo" — dark-first, Inter for UI text (SF Pro in the native app), JetBrains Mono for URLs and bodies, hairline dividers, one indigo accent (`#7C8CFF` dark / `#4F5BD5` light), status colors only where they carry meaning. The canvas covers Connect, Dashboard, Traffic (dark + light), Endpoints, Breakpoint editor, Rule builder, the notch Island, and the two Android screens. Its color tokens become `shared/design-tokens/tokens.json` (§1). A Google Stitch version was generated from the same brief ([design/DESIGN_BRIEF.md](design/DESIGN_BRIEF.md)) and rejected; its output stays in `design/stitch/` for reference only.

Design language: native macOS (SF Pro, SF Symbols, vibrancy sidebar), lots of whitespace, one accent color, status colors only where they carry meaning. Light and dark modes. Keyboard-first (⌘K command palette).

### 4.1 Main window: three panes
1. **Sidebar:** Devices (with live dot and request rate), Apps (bundle ID / package and app version), Domains → Endpoints tree, Saved Views, Rules (Breakpoints / Rewrite / Map Local / Map Remote / Block / Throttle) with on/off switches.
2. **Traffic list:** columns for Status pill, Method, Endpoint (templated path), Host, App, Device, Duration (inline mini bar), Size, Type icon, and a Rule badge (shows if a rewrite or map touched the request). Live tail with "pin to bottom". Group-by toggle: none / host / endpoint / app / device.
3. **Inspector:** Overview (timing waterfall: DNS / connect / TLS / TTFB / download), Request and Response tabs (Headers / Query / Body / Cookies / Raw), smart viewers (JSON tree with JSONPath copy, protobuf/gRPC, GraphQL with operation name and variables, images, HTML preview, hex), and actions (Breakpoint this, Map Local, Rewrite…, Repeat, Edit & Repeat, Copy as cURL / URLSession / OkHttp / fetch, Diff with…).

### 4.2 Filter bar and search
- **Chips:** `2xx` `3xx` `4xx` `5xx` `Failed` (connection or TLS errors) · Method · Content type · Device · App · **App version** (from the bundle via simctl, `dumpsys package`, or the User-Agent) · **API version** (auto-detected from `/v1/`, `Accept-Version`, `api-version` header) · Host · Has rule applied · Pinned/passthrough.
- **Search DSL** with autocomplete: `status:5xx host:api.acme.com method:POST path:/v2/orders* body:"timeout" duration:>800ms app:com.acme.ios version:3.4.*`. Plain text searches across URL, headers and bodies (FTS5).
- Saved Views: any filter can be saved and pinned to the sidebar.

### 4.3 Categorization (automatic)
- **Endpoint templating:** `/users/8231/orders/af3e…` becomes `/users/{id}/orders/{uuid}` (numeric, UUID, hash and date heuristics, plus user overrides). This drives grouping and the Endpoints view.
- **Category tags:** API, GraphQL, gRPC, Auth, Media, Static/CDN, Analytics (Firebase, Segment, Amplitude, Mixpanel…), Ads, Crash reporting, WebSocket. Tags come from a bundled host/path signature list that users can edit.
- **Endpoints dashboard:** per endpoint, count, error rate, p50/p95 latency, payload size, last seen, with sparklines. Click through to the filtered list. Sort by "slowest" or "most failing".

### 4.4 Dashboard home (once connected)
Cards: Live requests/sec, error rate, slowest endpoints, failing endpoints, top hosts, active rules, and a live tail of the most recent calls. Every card is a shortcut into a pre-filtered list.

### 4.5 Fast rule creation
- Right-click (or ⌘B) on any row → **Breakpoint this endpoint**, prefilled with method + host + templated path, request and/or response.
- **Map Local:** "Save response as mock" writes the body to a file and creates the rule in one step. The file is live-reloaded when edited in your editor.
- **Rewrite:** a visual rule builder (match → action list), including JSONPath set/remove for JSON bodies, header add/replace/remove, status override, query params and URL host swap.
- Rules are stored as human-readable **YAML** in `~/Library/Application Support/Relay/rules/` so they can be committed to a repo and shared.

### 4.6 Breakpoint editor
When a breakpoint hits, a focused editor appears (and the Island expands). It offers editable method/URL/headers/body with JSON validation and actions **Continue**, **Execute modified**, **Abort**, **Respond with mock** and **Continue all**. A timeout safety net (default 5 min) auto-continues so apps don't hang forever.

---

## 5. Notch "Dynamic Island"

- **Implementation:** a borderless, non-activating `NSPanel` at `.statusBar` level, anchored to `NSScreen.safeAreaInsets` / `auxiliaryTopLeftArea` to hug the notch. On Macs without a notch it becomes a floating pill at top-center. Spring animations via SwiftUI `matchedGeometryEffect`. Visible on all Spaces and over full-screen apps (`.canJoinAllSpaces`, `.fullScreenAuxiliary`). It can be hidden or set to show only during activity.
- **Compact state:** device icon + name · live request-rate pulse · red badge for new failures · a paused indicator when a breakpoint is waiting.
- **Expanded state (hover or click):**
  - Connected devices (switch the focused one)
  - Last 5 calls (status, method, endpoint, ms); click one to open it in the main window
  - Toggles: **Proxy**, **SSL decrypt**, **Breakpoints**, **Rewrite**, **Map Local**, **Map Remote**, **Throttle** (preset picker)
- **Breakpoint takeover:** when a breakpoint hits, the island expands with the endpoint and **Continue / Edit / Abort** buttons, so the user never has to find the window.
- **Alerts:** a short pulse for "pinning detected", "device disconnected" and "5xx spike".
- The **menu bar extra** mirrors the same toggles for people who turn the island off.

---

## 6. MCP support (every operation)

The MCP server lives in the core and maps 1:1 onto relay-control, so anything the UI does an agent can do, **on either platform**. `relay-mcp` is a stdio shim for local agents; streamable HTTP is served on localhost with a token.

**Multiple engines:** every tool takes an optional `engine` argument (`mac`, or an Android device serial). `list_engines` shows the Mac daemon plus any phone running Relay for Android that is reachable through `adb forward` or a paired LAN connection. An agent on your Mac can then drive the on-device debugger, for example by adding a mock on the phone and checking the app's next call.

**Tools**
| Group | Tools |
|---|---|
| Engines | `list_engines`, `engine_info(engine)` |
| Proxy | `proxy_status`, `proxy_start`, `proxy_stop`, `set_system_proxy`, `set_ssl_decrypt(hosts)`, `list_passthrough`, `set_throttle(preset|custom)` |
| Devices | `list_devices`, `connect_device(id)`, `disconnect_device`, `install_ca(device)`, `device_status`, `get_setup_qr` |
| Traffic | `list_transactions(filter DSL, limit, cursor)`, `get_transaction(id, include_bodies)`, `get_body(id, part, jsonpath?)`, `search(query)`, `endpoint_stats(filter)`, `wait_for_request(filter, timeout)`, `clear_session`, `export(format: har|curl|chls-like|postman)`, `import_har` |
| Breakpoints | `list_breakpoints`, `add_breakpoint(match, phase)`, `remove_breakpoint`, `list_paused`, `resume_paused(id, modifications?)`, `abort_paused` |
| Rewrite / Map | `list_rules`, `add_rewrite`, `add_map_local(match, file|inline_body, status, headers)`, `add_map_remote`, `add_block`, `toggle_rule`, `delete_rule`, `toggle_tool(rewrite|map_local|…)` |
| Compose | `send_request(spec)`, `repeat(id, n, concurrency)`, `edit_and_repeat(id, patch)` |
| Mocks | `record_scenario(name, filter)`, `replay_scenario(name)`, `list_scenarios` |
| Dev helpers | `device_logs(device, since, filter)`, `open_deeplink`, `diff_transactions(a, b)` |

**Resources:** `relay://transactions/{id}`, `relay://rules`, `relay://devices`, plus subscriptions that notify on new transactions matching a filter and on paused breakpoints.
**Prompts:** "Debug failing endpoint", "Mock this API for offline testing", "Find slow calls".
**Safety:** bodies are redacted by default for `Authorization`, cookies and fields matching a secret pattern (toggleable). Destructive tools (`clear_session`, `delete_rule`) are marked `destructiveHint`. Output is size-capped with cursors.

A Claude Code plugin/skill ships with it: `/relay` workflows plus `relay-mcp` registration via `relay mcp install`.

---

## 7. Extra features for iOS and Android developers

1. **Device logs on the same timeline.** Stream `simctl spawn … log stream` / `adb logcat` filtered to the app, and line logs up with requests. "Show logs around this request" (±2 s).
2. **Scenario mocks (record → replay).** Record a flow (for example login + feed), then replay it fully offline as a mock server. Great for UI tests and demos. Export it as a fixture folder.
3. **Network condition presets per device.** 3G, Edge, Lossy LTE, High latency, Offline, plus "Flaky" (random X% failures/timeouts). Can be scoped to one host or endpoint.
4. **Chaos rules.** Inject 500/429/timeouts at random or by schedule to test retry and backoff logic.
5. **GraphQL-aware everything.** Group, filter and breakpoint by `operationName`; persisted-query support.
6. **gRPC/Protobuf.** Import `.proto` or descriptor sets; reflection-based decoding when the server supports it; editable in breakpoints.
7. **Analytics inspector.** Decode batched events (Firebase/GA4, Segment, Amplitude, Mixpanel, AppsFlyer) into a readable event list. Useful for QA'ing tracking.
8. **Environment switcher.** One toggle maps `api.prod` → `api.staging` or `→ localhost:3000` on your Mac. Connect a device straight to a local dev server.
9. **Feature-flag override presets.** Saved rewrite bundles that patch config/flag endpoint responses (LaunchDarkly, Firebase Remote Config, custom).
10. **Pinning and ATS report.** Per app: which hosts are pinned, ATS exceptions found, cleartext HTTP calls, and weak TLS versions.
11. **Code export.** Copy as cURL, URLSession (Swift), OkHttp/Retrofit (Kotlin), Ktor and fetch. Generate Codable/Kotlin data classes from a JSON response.
12. **JWT and auth decoder.** Expiry countdown and a "token expired" highlight.
13. **Simulator and emulator quick actions** from the device card: open deep link, send a push payload (`simctl push`), clear app data (`pm clear`), restart the app, change locale, grant permissions.
14. **Request diff.** Compare two calls (or the same endpoint across app versions) side by side.
15. **Session sharing.** Export as HAR or a `.relay` bundle; import HAR and Charles `.chlsj` sessions.
16. **Assertions for CI.** `relay record --until "status:5xx"` and `relay assert "path:/v2/checkout status:200"` for use in XCUITest/Espresso pipelines.

---

## 8. Relay for Android (future app)

An on-device debugger: install it on the test phone, tap **Start**, and see and edit the traffic of any app on that phone. No Mac, cable or proxy settings needed. It is built on the same `relay-core`, so capture, TLS, rules, search and MCP behave exactly as on the Mac.

### 8.1 How capture works
- **`VpnService` (no root).** A local VPN receives every IP packet. `relay-netstack` (smoltcp) rebuilds TCP streams and hands them to the engine, which then opens real upstream sockets protected with `VpnService.protect()` so they don't loop back into the VPN.
- **Per-app capture:** `addAllowedApplication()` limits the VPN to the selected app(s), so other apps and the phone keep working normally and the battery cost stays low.
- **Exact attribution:** `ConnectivityManager.getConnectionOwnerUid()` (API 29+) maps each connection to the app's UID → package → version name. Below API 29, fall back to `/proc/net/tcp*` parsing.
- **DNS:** a built-in resolver in the VPN, plus host-based rules (DNS spoofing and blocking) at the DNS layer.
- **UDP/QUIC:** passed through by default and logged as flows. An optional "block QUIC" switch forces apps (Chrome/Cronet, YouTube) onto TCP so they can be decrypted.
- **Root mode (optional):** on rooted phones or emulators, install the CA in the system store (APEX on Android 14+) and optionally use iptables redirect instead of the VPN.

### 8.2 Certificate trust on the phone
- A guided flow: generate the CA → open the system "Install CA certificate" screen → verify trust automatically with a test request.
- Apps targeting API 24+ still ignore user CAs. **`relay-debug-lib`** (the Gradle plugin from the Mac plan) fixes that for your own debug builds, and the app explains it in one screen when it detects the issue.
- Pinning detection and one-click passthrough, the same as on the Mac.

### 8.3 Android UI (Jetpack Compose, same design tokens as the Mac)
- **Home:** big Start/Stop button, app picker (search installed apps, with icons), CA status, live counters.
- **Traffic list:** same columns, chips and search DSL as the Mac; group by endpoint; swipe actions for *Breakpoint*, *Mock*, *Repeat* and *Share as cURL*.
- **Inspector:** tabs for Overview/Request/Response, a JSON tree, and image and protobuf viewers.
- **Rules:** a phone-friendly version of the Mac rule builder. Map Local reads files from app storage or uses inline bodies. The rule YAML is the same format, so rules can be shared with the Mac via file, QR or sync.
- **Breakpoints on the phone:** when one hits, a heads-up notification (and optional bubble overlay) shows *Continue / Edit / Abort*. Edit opens a full-screen editor.

### 8.4 The Android "island"
The Mac's notch island has three Android equivalents, all driven by the same control events:
- **Ongoing notification** (required for the foreground service, so it does useful work): target app, requests/sec, error count, and action buttons *Pause*, *Rewrite on/off*, *Map Local on/off*.
- **Quick Settings tiles:** Capture, Rewrite, Map Local, Throttle.
- **Floating bubble overlay** (optional, needs `SYSTEM_ALERT_WINDOW`): a small pill over the app being debugged that shows the last call's status and expands into the last 5 calls plus toggles. This is the closest thing to a Dynamic Island on Android.

### 8.5 Working with the Mac
- **Pair-with-Mac** (see §3): the phone forwards captured traffic to the Mac engine, so you get the big-screen UI with on-device capture quality.
- **Remote view:** the Mac app can open the phone engine's session live (relay-control over `adb forward` or LAN) without moving the traffic.
- **Session handoff:** export a `.relay` session on the phone and open it on the Mac, or AirDrop/Nearby Share it to a teammate.
- **MCP:** an agent on the Mac targets the phone with `engine: <serial>` (see §6). An optional on-device MCP endpoint (localhost + token) is available for agents running on the phone or over LAN.

### 8.6 Android-specific developer features
- Logcat on the same timeline as requests, filtered to the target app (needs the `READ_LOGS` grant via a one-time `adb` command, or root).
- App controls: force-stop, clear data, and launch with a deep link (the Mac's device card does the same over adb).
- Network condition presets and chaos rules, applied on-device.
- Battery and data usage per endpoint, to catch chatty polling and oversized payloads.
- A background-request audit: calls made while the app is in the background, with the WorkManager/AlarmManager context where it can be inferred.

### 8.7 Android-specific risks
| Risk | Mitigation |
|---|---|
| Another VPN is active (only one VPN can run at a time) | Detect it and explain; offer Pair-with-Mac via `adb reverse` as the alternative |
| OEM battery killers (Xiaomi, Samsung, Huawei) stop the service | Foreground service + a battery-optimization exemption prompt + per-OEM guidance |
| Play Store policy on VPN/MITM apps | Ship via GitHub Releases + F-Droid first; Play listing is optional and limited to debugging your own apps |
| Throughput through a userspace TCP stack | Benchmark early (target ≥ 200 Mbps on a mid-range phone); keep smoltcp buffers tuned; zero-copy where possible |

---

## 9. Delivery phases

| Phase | Scope | Done when |
|---|---|---|
| **P0 — Spike (2 wk)** | Rust core: HTTP/1 + CONNECT + TLS MITM; UniFFI → a minimal Swift app; CA via simctl. **Also:** core cross-compiled for Android with a throwaway VpnService + smoltcp test app | Simulator Safari shows decrypted HTTPS through the Swift app, **and** the throwaway Android app shows one decrypted request. This proves both sides of the architecture before committing |
| **P1 — Engine MVP (3–4 wk)** | relayd + helper (dead-man switch), HTTP/2 both legs, WebSocket, store + FTS, relay-control API (versioned), rules YAML schema, CLI | Robustness checklist §2 green in CI; perf targets within 2×; core test suite also passes on the Android emulator |
| **P2 — App + devices (4 wk)** | Main window, inspector, filters/search DSL, endpoint templating, Connect Device for iOS sim, Android emu/USB, iPhone QR | Each device type goes from zero to decrypted traffic in under 60 s, measured |
| **P3 — Tools (4 wk)** | Breakpoints, Rewrite, Map Local/Remote, Block/Allow, Throttle, Compose/Repeat, DNS spoof, YAML rules, HAR import/export | Charles parity checklist (§0) complete except AMF/validator |
| **P4 — Island + MCP (2–3 wk)** | Notch island + menu bar, relay-mcp with all tools/resources, Claude Code plugin | Agent can connect a sim, add a mock and verify the app's call end to end via MCP |
| **P5 — Dev extras (ongoing)** | §7 features (logs timeline, scenarios, chaos, GraphQL/gRPC, analytics, environment switcher); iOS companion VPN app | Prioritized by user feedback |
| **P6 — Mac release** | Signing, notarization, Sparkle, Homebrew cask, docs site, contribution guide | Public Mac 1.0 |
| **P7 — Android alpha (5–6 wk)** | relay-netstack hardened, VpnService capture with per-app selection, CA flow, Compose traffic list + inspector, ongoing notification + Quick Settings tiles | Capture and decrypt a debug app built with relay-debug-lib on 3 OEMs (Pixel, Samsung, Xiaomi); 8-hour run without the service being killed |
| **P8 — Android tools + pairing (4 wk)** | Breakpoints (heads-up + editor), Rewrite / Map Local / Block / Throttle on the phone, bubble overlay, Pair-with-Mac, remote view from the Mac, multi-engine MCP | An agent on the Mac drives a mock on the phone via `engine: <serial>`; rules made on the Mac work unchanged on the phone |
| **P9 — Android release** | GitHub Releases APK, F-Droid, docs | Public Android 1.0 |

**P0 status (2026-11-20):** done on the Mac and Android CLI paths: Rust core with HTTP/1.1 + CONNECT + TLS MITM and a local CA (11 tests, including offline end-to-end TLS), UniFFI → SwiftUI Mac app showing live decrypted traffic, the CA trusted in the iOS Simulator via simctl, the engine cross-compiled to Android arm64 and decrypting on the emulator, and emulator → `adb reverse` → Mac app. iOS Simulator Safari verified through the macOS system proxy (settings restored afterwards). Finding: Apple pins `apple.com`, `icloud.com`, `mzstatic.com` and related domains system-wide (`CertificatePinning.plist`), so they are now in the default passthrough list instead of failing. **VpnService prototype (P0b, 2026-11-20): done.** `relay-netstack` (built on the `ipstack` user-space TCP/IP stack rather than hand-driven smoltcp: it hands back tokio TCP/UDP streams directly) feeds VPN connections into the same engine via a new transparent entry point that takes the hostname from TLS SNI or the HTTP Host header. On an Android 15 emulator, with only the target app routed into the VPN (Relay itself excluded, so there's no loop): HTTPS decrypted with no proxy settings, interception confirmed (a client trusting only public CAs is rejected), plain HTTP and DNS relayed, QUIC blocked so apps fall back to TCP. Throughput, verified against the engine log: 41 Mbit/s decrypted HTTPS (the emulator's internet cap; direct was 43) and about 168 Mbit/s plain HTTP locally against 202 direct. Findings for P1: (1) ipstack is about 25x slower on huge single writes, so writes into it are capped at 16 KB (7 to 168 Mbit/s); (2) whole-body buffering got Relay killed by Android's low-memory killer on a 200 MB download, which also tears down the VPN, so streaming is mandatory, not optional; (3) a foreground service + notification is needed so the VPN survives memory pressure. P1 follow-ups found in P0: log tunnels when they open (they only appear when they close today), read Apple's pinning plist at runtime. Known P0 shortcuts: bodies are buffered (no streaming), HTTP/1.1 only on the client leg, WebSocket upgrades are not relayed, brotli is not decoded for previews.

**P1 progress (2026-11-21):**
- ✅ **Streaming bodies.** Request and response bodies stream through a tee that keeps a bounded copy (256 KB) for the inspector, so memory per transaction is capped regardless of size. Responses reach the client as they arrive (the first SSE event in under 300 ms). Client and server aborts are recorded as interrupted. Verified: Mac Relay.app RSS flat at 106 MB during a 200 MB transfer to a slow client; Android VPN 3× 200 MB at about 164 Mbit/s, Relay alive, RSS flat at 140–142 MB, no low-memory kills (P0 was killed on the first one).
- ✅ **Live in-progress events.** Each transaction is emitted as `InProgress` when headers arrive or a tunnel opens, then again with its final state (same id; UIs replace the row). Passthrough tunnels and long streams are now visible while open.
- ✅ **HTTP/2 on both legs.** Relay offers h2 + http/1.1 to the app via ALPN and negotiates h2 upstream, translating freely between the two (h1↔h2 in any combination, multiplexed streams). h2c with prior knowledge is detected on plain/VPN traffic (it was previously tunneled blind), `te: trailers` and trailers pass through (gRPC), and each transaction records both legs' protocol (`HTTP/1.1 → HTTP/2` in the inspector). Found while testing live sites: a copied `Host` header on HTTP/2 upstream requests is rejected by Google's front end, so `Host` is no longer forwarded (hyper writes it, or `:authority`, from the URI), and the test origin now rejects it the same way. Verified: 36/36 live requests to Google, YouTube, GitHub, Wikipedia, Cloudflare and httpbin complete over h1 and h2 app legs; 6 parallel requests shared one h2 connection; Android VPN path decrypts with h2 upstream.
- ✅ **Tunnel accounting.** Passed-through tunnels now count bytes as they flow, so totals survive resets (they were logged as 0 B before), and once one side finishes the other gets a 5 s grace period (`tunnel_half_close_grace`) instead of waiting for the server's idle timeout (Apple's servers hold the connection for about 60 s). Verified on Mac against apple.com and icloud.com; the byte counts were also verified on Android 12 (Pixel 6 Pro emulator).
- ⚠️ **Open (Android):** through the VPN, apple.com tunnels did not close within 7 s after the app left, suggesting the user-space TCP stack (ipstack) may not surface the app's FIN. The emulator was replaced mid-investigation (a `Pixel_9_Pro` started by someone else), so this is unconfirmed. Investigate with a stable emulator.
- Next: WebSocket relay, store + FTS, relay-control API, rules YAML, relayd + privileged helper, Android foreground service.

Because the core is shared, P7–P8 are mostly UI and Android platform work. The engine features are already built and tested in P1–P3.

## 10. Testing strategy
- **Unit:** relay-core (filter DSL, templating, rule matching, netstack), at 90%+ coverage, run on macOS **and** on an Android emulator in CI.
- **Format compatibility:** shared `fixtures/` sessions and rules must parse identically from the Rust, Swift and Kotlin layers; old schema versions must still load.
- **Integration:** a local test server matrix (h1/h2, chunked, gzip/br/zstd, WebSocket, gRPC, SSE, big files, slow clients) run through the proxy, with byte-exact comparison.
- **Device E2E (CI on macOS runners):** boot an iOS Simulator and Android Emulator, run the sample apps, connect via the API, and assert traffic is captured and rules applied.
- **Chaos and soak:** 24 h at 500 req/s with random network flaps; memory must stay flat; the system proxy must always be restored after `kill -9`.
- **Fuzzing:** parsers, nightly.

## 11. Risks and mitigations
Android-specific risks are listed in §8.7.

| Risk | Mitigation |
|---|---|
| Rust ↔ Swift/Kotlin FFI complexity (async callbacks, large bodies) | Keep the FFI surface small: commands in, an event stream out, and bodies fetched lazily by ID. Use UniFFI async support; never pass big buffers through callbacks |
| Team needs Rust skills as well as Swift/Kotlin | The core is self-contained with strong tests; the UI shells can be built by Swift/Kotlin developers alone |
| Certificate pinning (Firebase, banking SDKs) | Auto-detect + passthrough; docs for debug-build pinning disable; optional Frida guidance is out of scope for the core |
| Android 14+ system CA store (APEX) | Mount-namespace injection on rooted emulators; debug-lib for real devices |
| QUIC/HTTP3 bypassing the proxy | Proxy mode forces TCP; transparent mode blocks UDP/443; flag it in the UI |
| No public notch API | Island positioned via screen safe-area insets; pill fallback; tested on every notch size |
| Privileged helper security | Minimal XPC surface, code-signature check of the caller, no arbitrary command execution |
| Scope creep | Ship P0–P4 first; §7 features are gated behind feedback |

## 12. Open decisions for you
1. ~~**Core language**~~ — **Decided (2026-11-20): Rust shared core** (`relay-core`) with UniFFI bindings for the Swift (Mac) and Kotlin (Android) apps. See §1.
2. **Name and license:** keep "Relay"? MIT or Apache-2.0?
3. **Minimum versions:** macOS 14 and Android 8.0 (recommended)?
4. **Companion apps:** the iOS companion VPN app and Relay for Android share `relay-netstack`. Should Android come before or after the iOS companion?
5. **Telemetry:** none at all (recommended for a privacy-sensitive tool)?
