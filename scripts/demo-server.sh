#!/usr/bin/env bash
# Runs the local mock API the Acme Shop demo apps talk to (no internet needed).
#   scripts/demo-server.sh [port]        default port 8099
# iOS Simulator reaches it at http://localhost:8099, the Android emulator at http://10.0.2.2:8099,
# and a real phone at http://<this Mac's address>:8099 (printed below).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/../core"
PORT="${1:-8099}"
LAN="$(ipconfig getifaddr en0 2>/dev/null || true)"
[ -n "$LAN" ] && echo "Real phone: set the demo's server to http://$LAN:$PORT"
exec cargo run -q -p relay-demo-server --release -- "$PORT"
