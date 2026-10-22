#!/usr/bin/env bash
# Builds a double-clickable scry.app bundle for macOS.
# Usage: scripts/make-macos-app.sh [output-dir]   (default: ./dist)
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-dist}"
APP="$OUT/scry.app"
MACOS="$APP/Contents/MacOS"
RES="$APP/Contents/Resources"

echo "Building release binary..."
cargo build --release

echo "Assembling $APP ..."
rm -rf "$APP"
mkdir -p "$MACOS" "$RES"
cp target/release/scry "$MACOS/scry"

# Icon: render a PNG, then build a multi-resolution .icns.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
target/release/scry icon "$TMP/icon.png"
ICONSET="$TMP/scry.iconset"
mkdir -p "$ICONSET"
for s in 16 32 64 128 256 512; do
  sips -z $s $s     "$TMP/icon.png" --out "$ICONSET/icon_${s}x${s}.png"      >/dev/null
  sips -z $((s*2)) $((s*2)) "$TMP/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$RES/scry.icns"

VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>scry</string>
  <key>CFBundleDisplayName</key><string>scry</string>
  <key>CFBundleExecutable</key><string>scry</string>
  <key>CFBundleIdentifier</key><string>tech.syscall.scry</string>
  <key>CFBundleIconFile</key><string>scry</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
PLIST

# Ad-hoc sign so macOS doesn't kill a freshly built, unsigned binary (SIGKILL / Gatekeeper).
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true

echo "Done: $APP"
echo "Run it:  open $APP     (or drag it to /Applications)"
