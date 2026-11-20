#!/usr/bin/env bash
# Builds relay-core for Android (arm64-v8a), copies the .so into the app, and generates Kotlin bindings.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$HOME/Library/Android/sdk/ndk/"* | sort -V | tail -1)}"
APP="$ROOT/android/relay-android/app"
ABI=arm64-v8a
TRIPLE=aarch64-linux-android

cd "$ROOT/core"
cargo ndk -t "$ABI" --platform 26 build -p relay-ffi --release
SO="$ROOT/core/target/$TRIPLE/release/librelay_ffi.so"

mkdir -p "$APP/src/main/jniLibs/$ABI"
cp "$SO" "$APP/src/main/jniLibs/$ABI/"

# Release .so files are stripped of UniFFI metadata; generate bindings from an unstripped host build.
cargo build -q -p relay-ffi
HOST_LIB="$( (ls "$ROOT/core/target/debug/librelay_ffi".{dylib,so} 2>/dev/null || true) | head -1)"
rm -rf "$APP/src/main/java/uniffi"
cargo run -q -p relay-ffi --bin uniffi-bindgen -- generate \
  --library "$HOST_LIB" --language kotlin --out-dir "$APP/src/main/java"
echo "Android native library and Kotlin bindings updated in $APP"
