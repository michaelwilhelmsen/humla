#!/usr/bin/env bash
# Cut a new Humla release: build the signed DMG + updater tarball, generate
# latest.json, tag the commit, push the tag, create a GitHub release, and
# upload all assets so existing installs auto-update.
#
# Prerequisites:
#   - .env.notarise present (Apple notarytool + Tauri updater key)
#   - gh CLI authenticated (`gh auth status`)
#   - Working tree clean
#   - Versions in package.json + tauri.conf.json + Cargo.toml all match
#   - That version is greater than the latest GitHub release
#
# Usage: pnpm release   (or: ./scripts/release.sh)
set -euo pipefail

cd "$(dirname "$0")/.."

# 1. Sanity checks.
if [[ ! -f .env.notarise ]]; then
  echo "error: .env.notarise missing — required for signing + notarisation" >&2
  exit 1
fi

if ! command -v gh >/dev/null; then
  echo "error: gh CLI not installed (brew install gh)" >&2
  exit 1
fi

if ! gh auth status >/dev/null 2>&1; then
  echo "error: gh not authenticated (gh auth login)" >&2
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree has uncommitted changes — commit or stash first" >&2
  git status --short
  exit 1
fi

# 2. Read + cross-check versions across the three files.
VERSION_PKG=$(node -p "require('./package.json').version")
VERSION_CONF=$(node -p "require('./src-tauri/tauri.conf.json').version")
VERSION_CARGO=$(awk -F\" '/^version *=/ {print $2; exit}' src-tauri/Cargo.toml)

if [[ "$VERSION_PKG" != "$VERSION_CONF" || "$VERSION_PKG" != "$VERSION_CARGO" ]]; then
  echo "error: version mismatch:" >&2
  echo "  package.json:       $VERSION_PKG" >&2
  echo "  tauri.conf.json:    $VERSION_CONF" >&2
  echo "  src-tauri/Cargo.toml: $VERSION_CARGO" >&2
  echo "  bump all three to the same value before releasing" >&2
  exit 1
fi

VERSION="$VERSION_PKG"
TAG="v$VERSION"
echo "release: $TAG"

# Refuse to overwrite an existing release.
if gh release view "$TAG" >/dev/null 2>&1; then
  echo "error: release $TAG already exists on GitHub — bump the version first" >&2
  exit 1
fi

# 3. Build (signs, notarises, staples, produces updater artifacts).
# Set SKIP_BUILD=1 to reuse existing artifacts on disk — useful when
# recovering from a release-script failure that happened *after* a
# successful build, so we don't rebuild and re-notarise needlessly.
if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
  echo "SKIP_BUILD=1 → reusing existing artifacts (no rebuild)"
else
  ./scripts/build-dmg.sh
fi

# 4. Locate artifacts.
DMG=$(ls -t src-tauri/target/release/bundle/dmg/*.dmg | head -n1)
TARBALL=$(ls -t src-tauri/target/release/bundle/macos/*.app.tar.gz | head -n1)
SIG_FILE=$(ls -t src-tauri/target/release/bundle/macos/*.app.tar.gz.sig | head -n1)

for f in "$DMG" "$TARBALL" "$SIG_FILE"; do
  if [[ ! -f "$f" ]]; then
    echo "error: expected artifact missing: $f" >&2
    exit 1
  fi
done

PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
DOWNLOAD_URL="https://github.com/michaelwilhelmsen/humla/releases/download/$TAG/$(basename "$TARBALL")"

# 5. Compose latest.json. Detect arch so the platform key is right —
# darwin-aarch64 for Apple Silicon, darwin-x86_64 for Intel.
ARCH=$(uname -m)
case "$ARCH" in
  arm64)  PLATFORM="darwin-aarch64" ;;
  x86_64) PLATFORM="darwin-x86_64"  ;;
  *) echo "unsupported arch $ARCH" >&2; exit 1 ;;
esac

LATEST_JSON="src-tauri/target/release/bundle/latest.json"

# Pass all the values into node via env so bash never interpolates into
# the JS source — that breaks on `${...}` JS template-style fragments.
VERSION="$VERSION" \
TAG="$TAG" \
PUB_DATE="$PUB_DATE" \
DOWNLOAD_URL="$DOWNLOAD_URL" \
PLATFORM="$PLATFORM" \
SIG_FILE="$SIG_FILE" \
LATEST_JSON="$LATEST_JSON" \
node <<'NODE_EOF'
const fs = require('fs');
const {
  VERSION, TAG, PUB_DATE, DOWNLOAD_URL, PLATFORM, SIG_FILE, LATEST_JSON,
} = process.env;
const manifest = {
  version: VERSION,
  notes: `See https://github.com/michaelwilhelmsen/humla/releases/tag/${TAG}`,
  pub_date: PUB_DATE,
  platforms: {
    [PLATFORM]: {
      signature: fs.readFileSync(SIG_FILE, 'utf8').trim(),
      url: DOWNLOAD_URL,
    },
  },
};
fs.writeFileSync(LATEST_JSON, JSON.stringify(manifest, null, 2));
NODE_EOF

echo "latest.json:"
cat "$LATEST_JSON"
echo

# 6. Tag and push.
git tag -a "$TAG" -m "Release $TAG"
git push origin "$TAG"

# 7. Create GitHub release with all assets.
gh release create "$TAG" \
  --title "$TAG" \
  --notes "Auto-generated release. Mac users on Apple Silicon: download the DMG, drag to Applications, right-click → Open on first launch.

Existing installs will offer to auto-update from the menu (Humla → Check for Updates…)." \
  "$DMG" \
  "$TARBALL" \
  "$SIG_FILE" \
  "$LATEST_JSON"

echo
echo "✅ Released $TAG"
echo "   https://github.com/michaelwilhelmsen/humla/releases/tag/$TAG"
echo
echo "Existing installs will auto-detect the update on next launch (or via Check for Updates…)."
