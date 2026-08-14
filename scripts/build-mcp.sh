#!/usr/bin/env bash
# Build the `humla-mcp` stdio MCP server (#172) and place it where Tauri expects,
# so it ships inside the bundle at Humla.app/Contents/MacOS/humla-mcp.
#
# Unlike the two Swift sidecars this is a second [[bin]] of the app's own Rust
# crate, so there is no source-hash skip: cargo already does exactly that, and
# duplicating it here would mean a stale binary whenever anything under
# src-tauri/src changed. What this script adds over a bare `cargo build` is the
# triple-suffixed copy Tauri's externalBin wants and the Developer ID + hardened
# runtime signature notarisation requires.
set -euo pipefail

cd "$(dirname "$0")/.."

ARCH=$(uname -m)
case "$ARCH" in
  arm64)  TRIPLE="aarch64-apple-darwin" ;;
  x86_64) TRIPLE="x86_64-apple-darwin" ;;
  *) echo "unsupported arch $ARCH"; exit 1 ;;
esac

mkdir -p src-tauri/binaries
DEST="src-tauri/binaries/humla-mcp-$TRIPLE"

# Chicken-and-egg: `humla-mcp` is declared in tauri.conf.json's externalBin, and
# tauri-build's build script fails the crate build when a declared sidecar is
# missing — including the build that produces this very binary. A zero-byte
# placeholder gets that first compile through; it is overwritten below, before
# anything bundles or signs it.
#
# The trap is not optional. Without it a failed `cargo build` leaves the empty file
# behind, and the next `tauri build` happily bundles and SIGNS a zero-byte
# `Contents/MacOS/humla-mcp` — a shipped server that dies the instant a client
# spawns it, with a Settings snippet pointing straight at it.
if [[ ! -f "$DEST" ]]; then
  : > "$DEST"
  trap 'rm -f "$DEST"' EXIT
fi

(
  cd src-tauri
  cargo build --release --bin humla-mcp
)

cp src-tauri/target/release/humla-mcp "$DEST"
# Past the only failure that could leave a placeholder behind.
trap - EXIT
chmod +x "$DEST"
xattr -cr "$DEST" || true

# One source of truth for the identity: tauri.conf.json.
IDENTITY=$(node -e "
  const c = require('./src-tauri/tauri.conf.json');
  process.stdout.write((c.bundle && c.bundle.macOS && c.bundle.macOS.signingIdentity) || '');
")

if [[ -n "$IDENTITY" ]] && security find-identity -v -p codesigning | grep -qF "$IDENTITY"; then
  echo "signing humla-mcp with: $IDENTITY"
  codesign --force --options runtime --sign "$IDENTITY" --timestamp "$DEST"
else
  echo "warning: Developer ID identity not in Keychain, falling back to ad-hoc signing"
  codesign --force --sign - "$DEST"
fi

echo "built: $DEST"
