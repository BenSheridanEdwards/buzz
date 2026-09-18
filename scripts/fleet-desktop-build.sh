#!/usr/bin/env bash
# Build and install the Fleet demo flavour of the desktop app.
#
# The same steps `just desktop-demo-build` runs, with the Fleet specifics
# pinned: a fixed demo build id (so the installed app keeps its bundle id,
# agents, membership and channels across rebuilds), the blue Fleet icon, the
# display name, and a stable code-signing identity so macOS does not ask for
# keychain access again on every rebuild.
#
#   scripts/fleet-desktop-build.sh            # build, sign, bundle
#   scripts/fleet-desktop-build.sh --install  # ... and install to /Applications, keeping the previous build
#
# Environment:
#   BUZZ_FLEET_BUILD_ID        demo build id (default: the installed Fleet app's)
#   BUZZ_FLEET_SIGN_IDENTITY   codesign identity name; "-" for ad-hoc (default: "Buzz Fleet Dev" when present, else "-")
#   BUZZ_FLEET_ROLLBACK_DIR    where --install parks the previous app (default: ~/Chief/Runbooks/rollback/fleet-app)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
TARGET="${BUZZ_FLEET_TARGET:-aarch64-apple-darwin}"
BUILD_ID="${BUZZ_FLEET_BUILD_ID:-51094c33a0d51d7c}"
NAME="Buzz Fleet"
if [[ -z "${BUZZ_FLEET_SIGN_IDENTITY:-}" ]]; then
  if security find-identity -v -p codesigning 2>/dev/null | grep -q '"Buzz Fleet Dev"'; then
    BUZZ_FLEET_SIGN_IDENTITY="Buzz Fleet Dev"
  else
    BUZZ_FLEET_SIGN_IDENTITY="-"
    echo "note: no 'Buzz Fleet Dev' identity in the keychain; signing ad-hoc (keychain will prompt on launch)" >&2
  fi
fi

[[ "$(uname -s)" == "Darwin" ]] || { echo "Fleet desktop builds are macOS only" >&2; exit 2; }
command -v node >/dev/null && command -v pnpm >/dev/null && command -v cargo >/dev/null \
  || { echo "activate hermit first: . ./bin/activate-hermit" >&2; exit 2; }

CONFIG="$(mktemp "${TMPDIR:-/tmp}/buzz-fleet-config.XXXXXX")"
trap 'rm -f "$CONFIG"' EXIT

echo "== sidecars ($TARGET)"
cargo build --release --target "$TARGET" \
  -p buzz-acp -p buzz-agent -p buzz-backend-kubernetes -p buzz-dev-mcp \
  -p git-credential-nostr -p buzz-cli
./scripts/bundle-sidecars.sh "$TARGET"

echo "== demo config (build id $BUILD_ID, Fleet icon)"
DEMO_CONFIG="$(BUZZ_DEMO_ICON_DIR=icons-fleet node desktop/scripts/demo-build-config.mjs Fleet "$CONFIG" "$BUILD_ID")"
SLUG="$(node -e 'console.log(JSON.parse(process.argv[1]).slug)' "$DEMO_CONFIG")"

echo "== tauri build"
pnpm install --silent
( cd desktop && BUZZ_BUILD_DEMO_SLUG="$SLUG" pnpm tauri build --features mesh-llm --target "$TARGET" --bundles app --config "$CONFIG" )

APP="desktop/src-tauri/target/$TARGET/release/bundle/macos/$NAME.app"
[[ -d "$APP" ]] || { echo "bundle missing: $APP" >&2; exit 1; }
PLIST="$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $NAME" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleName $NAME" "$PLIST"

echo "== sign as '$BUZZ_FLEET_SIGN_IDENTITY'"
codesign --force --deep --sign "$BUZZ_FLEET_SIGN_IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"
echo "built: $APP ($(git rev-parse --short HEAD))"

if [[ "${1:-}" == "--install" ]]; then
  DEST="/Applications/$NAME.app"
  ROLLBACK="${BUZZ_FLEET_ROLLBACK_DIR:-$HOME/Chief/Runbooks/rollback/fleet-app}"
  mkdir -p "$ROLLBACK"
  if [[ -d "$DEST" ]]; then
    PREV="$ROLLBACK/$NAME.app.$(date +%Y%m%d-%H%M%S)"
    mv "$DEST" "$PREV"
    echo "previous build parked at $PREV"
  fi
  cp -R "$APP" "$DEST"
  codesign --verify --deep --strict "$DEST"
  echo "installed: $DEST"
fi
