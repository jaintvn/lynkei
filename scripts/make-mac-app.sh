#!/usr/bin/env bash
# Builds the Swift app and wraps it in a minimal Lynkei.app bundle (unsigned, for local development).
# Usage: scripts/make-mac-app.sh [debug|release] [universal]   → apple/Lynkei/build/Lynkei.app
# `universal` builds for Apple Silicon and Intel (run scripts/build-apple.sh with UNIVERSAL=1 first).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${1:-debug}"
PKG="$ROOT/apple/Lynkei"

ARCHS=()
[ "${2:-}" = universal ] && ARCHS=(--arch arm64 --arch x86_64)
swift build --package-path "$PKG" -c "$CONFIG" ${ARCHS[@]+"${ARCHS[@]}"}
BIN="$(swift build --package-path "$PKG" -c "$CONFIG" ${ARCHS[@]+"${ARCHS[@]}"} --show-bin-path)/Lynkei"

APP="$PKG/build/Lynkei.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Lynkei"
cp "$PKG/Resources/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Lynkei</string>
  <key>CFBundleDisplayName</key><string>Lynkei</string>
  <key>CFBundleIdentifier</key><string>dev.lynkei.mac</string>
  <key>CFBundleExecutable</key><string>Lynkei</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>CFBundleDocumentTypes</key>
  <array><dict>
    <key>CFBundleTypeName</key><string>Lynkei session</string>
    <key>CFBundleTypeRole</key><string>Editor</string>
    <key>LSHandlerRank</key><string>Owner</string>
    <key>LSItemContentTypes</key><array><string>dev.lynkei.session</string></array>
  </dict></array>
  <key>UTExportedTypeDeclarations</key>
  <array><dict>
    <key>UTTypeIdentifier</key><string>dev.lynkei.session</string>
    <key>UTTypeDescription</key><string>Lynkei session</string>
    <key>UTTypeConformsTo</key><array><string>public.json</string></array>
    <key>UTTypeTagSpecification</key><dict><key>public.filename-extension</key><array><string>lynkei</string></array></dict>
  </dict></array>
</dict>
</plist>
PLIST
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
echo "$APP"
