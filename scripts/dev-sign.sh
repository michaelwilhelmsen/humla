#!/bin/sh
# Cargo runner hook for `pnpm tauri dev` (see src-tauri/.cargo/config.toml).
#
# Cargo links debug binaries ad-hoc, so their designated requirement is
# `cdhash H"..."` — a hash of that one build. macOS stores Keychain ACL trust
# against the DR, so every rebuild produced a binary the Keychain had never
# seen, and "Always Allow" had to be clicked again, forever.
#
# Signing each fresh build with the same Developer ID identity + identifier the
# release uses swaps that for an identity-based DR, which is byte-identical
# across rebuilds — so one "Always Allow" per Keychain item sticks, and dev
# shares that trust with the installed app.
#
# Fails soft: no identity on this machine (CI, a fresh clone) runs the binary
# unsigned, exactly as before. Only the app binary is signed — the runner also
# fires for `cargo test`, and test harnesses have no reason to pay for it.
set -e

case "$(basename "$1")" in
  humla) ;;
  *) exec "$@" ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
conf="$root/src-tauri/tauri.conf.json"

value_of() {
  sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$conf" | head -1
}

identity=$(value_of signingIdentity)
identifier=$(value_of identifier)

if [ -n "$identity" ] && security find-identity -v -p codesigning 2>/dev/null | grep -qF "$identity"; then
  codesign --force --sign "$identity" \
    --identifier "$identifier" \
    --entitlements "$root/src-tauri/entitlements.plist" \
    "$1" >/dev/null 2>&1 || echo "dev-sign: codesign failed, running unsigned" >&2
fi

exec "$@"
