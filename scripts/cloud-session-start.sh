#!/usr/bin/env bash
# SessionStart hook (.claude/settings.json). Readies a Claude Code cloud session
# to build and test everything that can run off a Mac; see docs/agents/cloud.md.
# Does nothing outside a cloud session, so it is safe on every machine and fork.
set -uo pipefail

[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0
[ "$(uname -s)" = "Linux" ] || exit 0

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/..}" || exit 0
log="${TMPDIR:-/tmp}/humla-session-start.log"
: > "$log"
notes=()

as_root() {
  if [ "$(id -u)" = 0 ]; then "$@"; elif command -v sudo >/dev/null; then sudo -n "$@"; else return 1; fi
}

# Tauri's Linux build links against these. An environment setup script that
# installs them (docs/agents/cloud.md) is cached in the VM snapshot and makes
# this a no-op; without one this costs about half a minute per session.
if ! pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
  if as_root apt-get update >>"$log" 2>&1 \
    && as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      pkg-config cmake clang libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
      libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev >>"$log" 2>&1; then
    notes+=("installed Tauri's Linux libraries")
  else
    notes+=("could not install Tauri's Linux libraries, so the humla crate won't build (log: $log)")
  fi
fi

# pnpm at the version package.json pins.
if command -v corepack >/dev/null; then
  corepack enable pnpm >>"$log" 2>&1 || as_root corepack enable pnpm >>"$log" 2>&1 || true
fi
if pnpm install --frozen-lockfile >>"$log" 2>&1; then
  notes+=("pnpm $(pnpm -v) dependencies installed")
else
  notes+=("pnpm install failed (log: $log)")
fi

# The humla crate's rust-version.
need=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' src-tauri/Cargo.toml | head -1)
have=$(rustc --version 2>/dev/null | awk '{print $2}')
if [ -z "$have" ] || [ "$(printf '%s\n%s\n' "$need" "$have" | sort -V | head -1)" != "$need" ]; then
  if command -v rustup >/dev/null && rustup toolchain install stable --profile minimal >>"$log" 2>&1 \
    && rustup default stable >>"$log" 2>&1; then
    have=$(rustc --version | awk '{print $2}')
  else
    notes+=("Rust ${have:-missing} is older than the $need the crate needs (log: $log)")
  fi
fi

# tauri-build refuses to compile without a file for every externalBin. The real
# sidecars are macOS binaries; empty stand-ins are enough to build and test.
if triple=$(rustc -vV 2>/dev/null | sed -n 's/^host: //p') && [ -n "$triple" ]; then
  mkdir -p src-tauri/binaries
  for bin in audio-capture speaker-diarize humla-mcp; do
    [ -e "src-tauri/binaries/$bin-$triple" ] || : > "src-tauri/binaries/$bin-$triple"
  done
fi

summary="Rust ${have:-missing}"
for note in "${notes[@]}"; do summary="$summary; $note"; done
echo "Humla cloud session: $summary."
echo "Sidecars, Metal, signing and running the app need a Mac: see docs/agents/cloud.md."
exit 0
