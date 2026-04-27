#!/usr/bin/env bash
# Build the Swift audio-capture sidecar and place it where Tauri expects.
set -euo pipefail

cd "$(dirname "$0")/.."

# Detect host triple
ARCH=$(uname -m)
case "$ARCH" in
  arm64)  TRIPLE="aarch64-apple-darwin" ;;
  x86_64) TRIPLE="x86_64-apple-darwin" ;;
  *) echo "unsupported arch $ARCH"; exit 1 ;;
esac

mkdir -p src-tauri/binaries

(
  cd audio-capture
  swift build -c release
)

DEST="src-tauri/binaries/audio-capture-$TRIPLE"
cp audio-capture/.build/release/audio-capture "$DEST"
chmod +x "$DEST"

# Strip quarantine/provenance xattrs that make Gatekeeper hang on each spawn,
# then re-stamp an ad-hoc signature so macOS treats it as a known local binary.
xattr -cr "$DEST" || true
codesign --force --sign - "$DEST"

echo "built: $DEST"
