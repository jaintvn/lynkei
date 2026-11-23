#!/usr/bin/env bash
# Builds relay-core for macOS (Apple Silicon; UNIVERSAL=1 adds Intel), generates Swift bindings, and packages RelayFFI.xcframework
# into apple/RelayKit so the Swift packages can link it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
TARGET=aarch64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=14.0
KIT="$ROOT/apple/RelayKit"
WORK="$ROOT/core/target/apple-bindings"

cd "$ROOT/core"
cargo build -p relay-ffi --release --target "$TARGET"
LIB_DIR="$ROOT/core/target/$TARGET/release"
STATIC="$LIB_DIR/librelay_ffi.a"
if [ "${UNIVERSAL:-0}" = 1 ]; then
  cargo build -p relay-ffi --release --target x86_64-apple-darwin
  STATIC="$ROOT/core/target/librelay_ffi_universal.a"
  lipo -create "$LIB_DIR/librelay_ffi.a" "$ROOT/core/target/x86_64-apple-darwin/release/librelay_ffi.a" -output "$STATIC"
fi

rm -rf "$WORK" && mkdir -p "$WORK/headers" "$WORK/swift"
cargo run -q -p relay-ffi --bin uniffi-bindgen -- generate \
  --library "$LIB_DIR/librelay_ffi.dylib" --language swift --out-dir "$WORK/swift"

mv "$WORK/swift/"*.h "$WORK/headers/"
# SwiftPM/XCFramework expects the module map to be called module.modulemap.
mv "$WORK/swift/"*.modulemap "$WORK/headers/module.modulemap"

rm -rf "$KIT/RelayFFI.xcframework"
xcodebuild -create-xcframework \
  -library "$STATIC" -headers "$WORK/headers" \
  -output "$KIT/RelayFFI.xcframework" >/dev/null

mkdir -p "$KIT/Sources/RelayKit/Generated"
cp "$WORK/swift/"*.swift "$KIT/Sources/RelayKit/Generated/"
echo "RelayFFI.xcframework and Swift bindings updated in $KIT"
