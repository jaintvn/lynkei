#!/usr/bin/env bash
# Builds lynkei-core for macOS (Apple Silicon; UNIVERSAL=1 adds Intel), generates Swift bindings, and packages LynkeiFFI.xcframework
# into apple/LynkeiKit so the Swift packages can link it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
TARGET=aarch64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=14.0
KIT="$ROOT/apple/LynkeiKit"
WORK="$ROOT/core/target/apple-bindings"

cd "$ROOT/core"
cargo build -p lynkei-ffi --release --target "$TARGET"
LIB_DIR="$ROOT/core/target/$TARGET/release"
STATIC="$LIB_DIR/liblynkei_ffi.a"
if [ "${UNIVERSAL:-0}" = 1 ]; then
  cargo build -p lynkei-ffi --release --target x86_64-apple-darwin
  STATIC="$ROOT/core/target/liblynkei_ffi_universal.a"
  lipo -create "$LIB_DIR/liblynkei_ffi.a" "$ROOT/core/target/x86_64-apple-darwin/release/liblynkei_ffi.a" -output "$STATIC"
fi

rm -rf "$WORK" && mkdir -p "$WORK/headers" "$WORK/swift"
cargo run -q -p lynkei-ffi --bin uniffi-bindgen -- generate \
  --library "$LIB_DIR/liblynkei_ffi.dylib" --language swift --out-dir "$WORK/swift"

mv "$WORK/swift/"*.h "$WORK/headers/"
# SwiftPM/XCFramework expects the module map to be called module.modulemap.
mv "$WORK/swift/"*.modulemap "$WORK/headers/module.modulemap"

rm -rf "$KIT/LynkeiFFI.xcframework"
xcodebuild -create-xcframework \
  -library "$STATIC" -headers "$WORK/headers" \
  -output "$KIT/LynkeiFFI.xcframework" >/dev/null

mkdir -p "$KIT/Sources/LynkeiKit/Generated"
cp "$WORK/swift/"*.swift "$KIT/Sources/LynkeiKit/Generated/"
echo "LynkeiFFI.xcframework and Swift bindings updated in $KIT"
