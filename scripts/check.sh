#!/usr/bin/env bash
# Runs the same checks as CI, on this Mac (free): Rust fmt, clippy and tests, then the Mac app build.
#   scripts/check.sh          everything
#   scripts/check.sh core     only the Rust checks (faster)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

cd "$ROOT/core"
step "cargo fmt --check"
cargo fmt --all --check
step "cargo clippy (warnings are errors)"
cargo clippy --workspace --all-targets -- -D warnings
step "cargo test"
cargo test --workspace

if [ "${1:-all}" != "core" ]; then
  cd "$ROOT"
  step "XCFramework + Swift bindings"
  scripts/build-apple.sh
  step "Mac app build"
  swift build --package-path apple/Relay
fi

printf '\n\033[1;32mAll checks passed.\033[0m\n'
