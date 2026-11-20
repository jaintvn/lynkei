# Relay: UI design brief

This one brief goes to both **Google Stitch** and **Claude Design** so the two results can be compared fairly. Both get the same screens, content and sample data.

## Product
Relay is an open-source macOS app (with an Android app later) for inspecting and modifying the network traffic of iOS and Android apps during development. It's a modern replacement for Charles Proxy. Users are mobile developers and QA engineers, who spend hours a day in it next to Xcode or Android Studio.

## Visual direction
- **Modern and minimal.** It should feel native to macOS (Sonoma and later): SF Pro / SF Mono, SF Symbols-style line icons, a translucent sidebar, 8 pt grid, 10–12 px radii, hairline separators, no heavy borders or gradients.
- **Dark mode first** (most developers use it), with a light mode that also looks good.
- **One accent color** (for example a vivid electric blue or violet). Status colors appear only where they carry meaning: 2xx green, 3xx blue-grey, 4xx amber, 5xx red, failed (connection/TLS) magenta, paused-at-breakpoint orange.
- **Information-dense but calm.** Tables are compact (28 px rows), monospace for URLs, paths and bodies, and visual weight goes to what changed or failed.
- **Fast to scan.** Status pills, method badges, inline duration bars, endpoint templating shown as `/users/{id}`.
- Keyboard-first hints (⌘K palette, ⌘B breakpoint), shown subtly.

## Sample data (use this everywhere)
- Devices: **iPhone 17 Pro Simulator (iOS 26.4)** — connected · **Pixel 9 Emulator (Android 16)** — connected · **Jain's iPhone** — needs CA trust
- Apps: **Acme Shop** `com.acme.shop` v3.4.1 (build 812) on iOS and Android
- Hosts: `api.acmeshop.com`, `auth.acmeshop.com`, `cdn.acmeshop.com`, `graphql.acmeshop.com`, `firebaselogging-pa.googleapis.com`, `api.segment.io`
- Requests (mix):
  - `GET /v2/products?category=shoes&page=2` · 200 · 142 ms · 18.4 KB · JSON
  - `GET /v2/products/{id}` · 200 · 88 ms
  - `POST /v2/cart/items` · 201 · 230 ms
  - `POST /v2/checkout` · **500** · 1.84 s, highlighted as failing
  - `POST /graphql` operation `GetOrderHistory` · 200 · 310 ms
  - `POST /oauth/token` · 401 · 64 ms
  - `GET /images/p/8231@3x.webp` · 200 · CDN · 212 KB
  - `POST /v1/batch` (Segment analytics) · 200
  - `GET /v2/recommendations` · paused at breakpoint
  - `CONNECT pinned.bank-sdk.com:443` · failed — certificate pinning detected
- Active rules: Map Local `GET /v2/recommendations → recommendations_empty.json`; Rewrite "Force feature flag new_checkout=true"; Throttle "LTE – lossy" on Pixel 9.

## Screens to design

### Mac app (1440 × 900 window)
1. **Connect a device (onboarding sheet).** Auto-detected device cards (iOS Simulator, Android Emulator, Android USB, iPhone over Wi-Fi), each with a single "Connect" button and a three-step status checklist: *Proxy · Certificate trusted · Traffic seen*. The iPhone card shows a QR code with "Scan with your iPhone camera". The tone is effortless: "Relay sets everything up for you."
2. **Dashboard (home after connecting).** Cards: live requests/sec sparkline, error rate, slowest endpoints, failing endpoints, top hosts, active rules (with toggles), and a live tail of the latest calls. A device switcher appears at the top.
3. **Traffic (main working screen).** Three panes:
   - Sidebar: Devices, Apps, Domains → endpoints tree, Saved views, Rules with on/off switches.
   - Center: filter chips (2xx 3xx 4xx 5xx Failed · Method · Type · Device · App version · API version), a search field showing a DSL example `status:5xx host:api.acmeshop.com duration:>800ms`, and a traffic table with columns Status, Method, Endpoint, Host, App, Duration (inline bar), Size, Type, Rule badge.
   - Right inspector: the selected request `POST /v2/checkout` (500), with tabs Overview / Request / Response / Timing, a timing waterfall (DNS · Connect · TLS · TTFB · Download), a pretty JSON error body, and action buttons: Breakpoint, Map Local, Rewrite, Repeat, Copy as cURL.
4. **Endpoints.** An aggregated table per templated endpoint: calls, error %, p50/p95 latency, avg size, last seen, and a sparkline. Sorted by "most failing".
5. **Breakpoint editor.** The request `GET /v2/recommendations` is paused. Editable method, URL, headers and JSON body; buttons Continue, Execute modified, Respond with mock, Abort; an auto-continue countdown.
6. **Rule builder (Rewrite / Map Local).** A "When" section (method, host, templated path, device, app) and a "Then" action list (set JSON path, replace header, override status, serve local file). There's a live preview of which recent requests match.

### Mac "Dynamic Island" (notch overlay, top-center of screen)
7. Three states on one artboard:
   - **Compact:** device glyph, "Pixel 9", a pulsing 12 req/s, red badge "1".
   - **Expanded:** connected devices, the last 5 calls, and toggles Proxy · SSL · Breakpoints · Rewrite · Map Local · Throttle.
   - **Breakpoint hit:** "Paused: GET /v2/recommendations" with Continue / Edit / Abort.

### Android app (future, phone 412 × 915)
8. **Home:** big Start/Stop capture button, target app picker (Acme Shop), CA status, live counters.
9. **Traffic list** with filter chips and swipe actions, plus a persistent notification mock showing the toggles.

## Deliverable expectations
- The same content on every screen, so the two tools can be compared on visual quality only.
- Dark mode for all screens; light mode at least for the Traffic screen.
- Realistic density. This is a pro tool, not a marketing page.
