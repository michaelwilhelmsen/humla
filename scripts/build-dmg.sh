#!/usr/bin/env bash
# Build a distributable Humla.dmg for sharing with teammates.
# Output lands in src-tauri/target/release/bundle/dmg/.
#
# When .env.notarise is present, the build is signed with the Developer ID
# from tauri.conf.json and notarised via App Store Connect API key, and
# the auto-updater artifacts (.app.tar.gz + .sig) are produced for the
# release flow. Without .env.notarise, the build is just Developer ID
# signed (no notarisation), and recipients will need to right-click → Open
# on first launch.
set -euo pipefail

cd "$(dirname "$0")/.."

# Source secrets if present. The file exports both:
#   APPLE_API_KEY / APPLE_API_ISSUER / APPLE_API_KEY_PATH (notarytool)
#   TAURI_SIGNING_PRIVATE_KEY / _PASSWORD (auto-updater minisign)
if [[ -f .env.notarise ]]; then
  # shellcheck disable=SC1091
  source .env.notarise
  if [[ ! -f "${APPLE_API_KEY_PATH:-}" ]]; then
    echo "error: APPLE_API_KEY_PATH=$APPLE_API_KEY_PATH does not exist" >&2
    exit 1
  fi
  if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" && ! -f "${TAURI_SIGNING_PRIVATE_KEY}" ]]; then
    echo "error: TAURI_SIGNING_PRIVATE_KEY=$TAURI_SIGNING_PRIVATE_KEY does not exist" >&2
    exit 1
  fi
  echo "notarisation: enabled (key $APPLE_API_KEY)"
  echo "updater signing: enabled"
else
  echo "notarisation: skipped (no .env.notarise)"
  echo "updater signing: skipped"
fi

./scripts/build-sidecar.sh
./scripts/build-metallib.sh
# The MCP server (#172) ships inside the bundle as an externalBin, so it must
# exist before `tauri build` runs or the bundler fails on a missing sidecar.
./scripts/build-mcp.sh

pnpm tauri build

OUT_DIR="src-tauri/target/release/bundle/dmg"
DMG=$(ls -t "$OUT_DIR"/*.dmg 2>/dev/null | head -n1 || true)

if [[ -z "$DMG" ]]; then
  echo "build finished but no DMG found in $OUT_DIR" >&2
  exit 1
fi

# Tauri's bundler notarises and staples the .app inside the DMG, but doesn't
# submit the DMG itself for its own ticket. Stapling the DMG too means
# Gatekeeper accepts it cleanly when downloaded directly (web links etc.),
# not just after the .app is extracted.
if [[ -n "${APPLE_API_KEY:-}" ]]; then
  if xcrun stapler validate "$DMG" >/dev/null 2>&1; then
    echo "DMG already stapled, skipping"
  else
    echo "Submitting DMG for notarisation…"
    xcrun notarytool submit "$DMG" \
      --key "$APPLE_API_KEY_PATH" \
      --key-id "$APPLE_API_KEY" \
      --issuer "$APPLE_API_ISSUER" \
      --wait
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
  fi
fi

echo
echo "DMG ready: $DMG"
if [[ -n "${APPLE_API_KEY:-}" ]]; then
  echo "Signed, notarised, and stapled. Teammates can double-click to install."
else
  echo "Signed but not notarised. First launch needs right-click → Open."
fi

# Surface the updater artifacts so the release script can find them.
MACOS_DIR="src-tauri/target/release/bundle/macos"
SIG_FILE=$(ls -t "$MACOS_DIR"/*.app.tar.gz.sig 2>/dev/null | head -n1 || true)
TARBALL=$(ls -t "$MACOS_DIR"/*.app.tar.gz 2>/dev/null | head -n1 || true)
if [[ -n "$SIG_FILE" && -n "$TARBALL" ]]; then
  echo "updater artifacts:"
  echo "  $TARBALL"
  echo "  $SIG_FILE"
fi
