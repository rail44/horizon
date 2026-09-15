#!/usr/bin/env bash
# Assemble a minimal macOS .app bundle around the freshly built workspace
# binaries and launch Horizon from it.
#
# Why this exists: UNUserNotificationCenter — the only macOS notification
# API that still routes (NSUserNotification stopped being delivered in
# macOS 26) — refuses to work for a bare executable. The process must run
# from an .app bundle with a CFBundleIdentifier and be code-signed; an
# ad-hoc signature suffices. A bare `just dev` therefore never sees a
# notification banner; this wrapper provides exactly the required shape
# (no Developer ID, no notarization) while reusing the normal target/
# build. The desktop-notification permission the user grants is keyed to
# CFBundleIdentifier below — changing it resets the grant and re-prompts.
#
# Every daemon is copied next to the main binary because
# `horizon_wire::resolve_daemon_binary` looks for them beside the running
# executable (then falls back to PATH).
#
# Usage: ./scripts/dev-bundle.sh [horizon args...]
#   PROFILE=release ./scripts/dev-bundle.sh   for a release-profile bundle
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PROFILE="${PROFILE:-debug}"
BIN_DIR="target/$PROFILE"
APP="$BIN_DIR/Horizon.app"

BINARIES=(horizon horizon-agentd horizon-terminald horizon-logd horizon-sandbox-helper horizon-sandbox-network-probe)
for binary in "${BINARIES[@]}"; do
  if [[ ! -x "$BIN_DIR/$binary" ]]; then
    echo "missing $BIN_DIR/$binary — run 'cargo build --workspace' first" >&2
    exit 1
  fi
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.rail44.horizon</string>
    <key>CFBundleName</key>
    <string>Horizon</string>
    <key>CFBundleDisplayName</key>
    <string>Horizon</string>
    <key>CFBundleExecutable</key>
    <string>horizon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

for binary in "${BINARIES[@]}"; do
  cp "$BIN_DIR/$binary" "$APP/Contents/MacOS/"
done

codesign --force --deep --sign - "$APP"

exec "$APP/Contents/MacOS/horizon" "$@"
