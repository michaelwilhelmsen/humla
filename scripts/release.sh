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

# 4. Locate artifacts — by VERSION and by payload, never by "newest on disk".
#
# This used to be three `ls -t | head -n1` calls, which is a trap that nearly
# shipped during v0.52.0. Tauri bundles in the order app → DMG → updater
# tarball, so a failure in the DMG step (that one was macOS refusing
# `bundle_dmg.sh` its Finder automation prompt) leaves the app rebuilt but the
# updater tarball still belonging to the PREVIOUS release. `ls -t` then picks
# that stale tarball happily, and since `latest.json` takes its version from
# these files' surroundings rather than from their contents, the release
# publishes as the new version carrying the old payload — with a signature that
# verifies, because it is the old payload's own signature. Every install would
# have taken the update and quietly moved backwards. Nothing about the release
# would have looked wrong.
#
# So: demand each artifact by name where the name carries the version, and read
# the version out of the payload where it does not.

# The DMG's filename carries the version; only the arch suffix varies
# (aarch64 / x64), so glob that much and insist on exactly one match.
shopt -s nullglob
DMG_MATCHES=(src-tauri/target/release/bundle/dmg/Humla_"${VERSION}"_*.dmg)
shopt -u nullglob
if (( ${#DMG_MATCHES[@]} != 1 )); then
  echo "error: expected exactly one DMG named for $VERSION, found ${#DMG_MATCHES[@]}" >&2
  printf '  %s\n' "${DMG_MATCHES[@]:-(none)}" >&2
  echo "  run ./scripts/build-dmg.sh, and delete any DMG from another version" >&2
  exit 1
fi
DMG="${DMG_MATCHES[0]}"

# The updater tarball's name carries NO version — `Humla.app.tar.gz`, release
# after release — so its name proves nothing and its mtime is a guess. Read the
# version out of the payload that is actually about to ship.
TARBALL="src-tauri/target/release/bundle/macos/Humla.app.tar.gz"
SIG_FILE="$TARBALL.sig"

for f in "$DMG" "$TARBALL" "$SIG_FILE"; do
  if [[ ! -f "$f" ]]; then
    echo "error: expected artifact missing: $f" >&2
    exit 1
  fi
done

TARBALL_VERSION=$(
  tar -xOzf "$TARBALL" "Humla.app/Contents/Info.plist" 2>/dev/null \
    | plutil -extract CFBundleShortVersionString raw -o - -- - 2>/dev/null
) || true
if [[ "$TARBALL_VERSION" != "$VERSION" ]]; then
  echo "error: the updater payload is not $VERSION" >&2
  echo "  $TARBALL says: ${TARBALL_VERSION:-(unreadable)}" >&2
  echo "  this is the stale-tarball trap — a DMG-step failure leaves the previous" >&2
  echo "  release's tarball in place. Rebuild rather than reuse:" >&2
  echo "    rm -f $TARBALL $SIG_FILE && ./scripts/build-dmg.sh" >&2
  exit 1
fi

# The signature is made from the tarball. One older than what it signs is
# signing a payload that is no longer there, and every install rejects the
# download.
if [[ "$SIG_FILE" -ot "$TARBALL" ]]; then
  echo "error: $SIG_FILE is older than the tarball it signs — rebuild" >&2
  exit 1
fi

echo "artifacts for $VERSION:"
echo "  $DMG"
echo "  $TARBALL (payload reports $TARBALL_VERSION)"
echo "  $SIG_FILE"

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
#
# The BRANCH goes first, then the tag. Pushing only the tag — which is what this
# did until v0.42.0 — leaves the version bump sitting unpushed on the local branch
# while the release itself looks complete: the tag resolves, the assets upload, the
# updater works. What breaks is everything that reads the branch. GitHub's main
# says the old version, a fresh clone can't reproduce the release it's tagged at,
# and the next release's "is the version bumped past the latest release?" guard
# compares against a tree that never got the last bump.
#
# Order matters for the same reason: a tag pushed ahead of its branch points at a
# commit the remote branch doesn't contain yet, so anyone fetching in that window
# sees a tag hanging off nothing.
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
git push origin "$BRANCH"
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
