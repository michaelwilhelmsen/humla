#!/usr/bin/env bash
# Generate the full Tauri icon set from src-tauri/icons/source.png.
# Applies a macOS squircle mask + transparent padding before delegating
# to `pnpm tauri icon`, so every output (icns, ico, png variants) inherits
# the proper Apple icon shape.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="${1:-$ROOT/src-tauri/icons/source.png}"
STAGED="$ROOT/src-tauri/icons/.app-icon-staged.png"

if [ ! -f "$SOURCE" ]; then
    echo "no source image at $SOURCE" >&2
    exit 1
fi

echo "[icon] masking $SOURCE -> $STAGED"
swift "$ROOT/scripts/squircle-icon.swift" "$SOURCE" "$STAGED"

echo "[icon] generating tauri icon set"
cd "$ROOT"
pnpm tauri icon "$STAGED"

rm -f "$STAGED"
echo "[icon] done"
