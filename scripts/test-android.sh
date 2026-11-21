#!/usr/bin/env bash
# Runs the core test suites on a connected Android device or emulator (arm64).
# Usage: scripts/test-android.sh            (uses the first device `adb devices` lists)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$HOME/Library/Android/sdk/ndk/"* | sort -V | tail -1)}"
ADB="${ADB:-$HOME/Library/Android/sdk/platform-tools/adb}"
CRATES=(relay-model relay-proxy relay-store relay-netstack relay-control)
# Tests that read files from the source tree (not present on the device).
SKIP=(--skip checked_in_json_schema --skip shipped_example)

cd "$ROOT/core"
args=(); for c in "${CRATES[@]}"; do args+=(-p "$c"); done
cargo ndk -t arm64-v8a --platform 26 test --no-run "${args[@]}"

# Newest test executable per test target (cargo's build-dir layout: build/<crate>/<hash>/out/).
bins=()
for t in relay_model relay_proxy e2e relay_store relay_netstack close relay_control control; do
  f=$(find target/aarch64-linux-android/debug/build -path "*/out/${t}-*" -type f -perm -u+x 2>/dev/null \
      | grep -vE '\.(d|rlib|rmeta|so|o)$' | xargs ls -t 2>/dev/null | head -1 || true)
  [ -n "$f" ] && bins+=("$f")
done

DEV=/data/local/tmp/relay-tests
"$ADB" shell mkdir -p "$DEV"
failed=0
for b in "${bins[@]}"; do
  n=$(basename "$b")
  "$ADB" push "$b" "$DEV/" >/dev/null
  out=$("$ADB" shell "cd $DEV && chmod 755 $n && TMPDIR=$DEV ./$n --test-threads 4 ${SKIP[*]} 2>&1; echo EXIT=\$?")
  printf '%-34s %s\n' "$n" "$(echo "$out" | grep -E '^test result' | sed 's/; finished.*//')"
  if ! echo "$out" | grep -q 'EXIT=0'; then
    failed=1
    echo "$out" | grep -E 'panicked at|^---- ' | head -10
  fi
done
exit $failed
