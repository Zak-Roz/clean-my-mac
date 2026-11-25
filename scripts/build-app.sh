#!/bin/bash
# Builds clean-my-mac.app with the `cmm` CLI inside it (Contents/MacOS/cmm) and signs it ad hoc.
#
#   scripts/build-app.sh [--universal]
#
#   --universal   build one app for Apple silicon and Intel (needs the aarch64-apple-darwin and
#                 x86_64-apple-darwin Rust targets: `rustup target add …`); by default the app is
#                 built for this Mac's architecture only
#
# Progress goes to stderr; the only line on stdout is the path of the built app:
#   target/release/bundle/macos/clean-my-mac.app                         (default)
#   target/universal-apple-darwin/release/bundle/macos/clean-my-mac.app  (--universal)
# The release workflow re-signs that app with the release certificate (scripts/sign-app.sh).
# Works with the macOS /bin/bash (3.2).
set -euo pipefail

APP_NAME="clean-my-mac.app"

die() { printf 'build-app: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,14s/^# \{0,1\}//p' "$0"; }

UNIVERSAL=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --universal) UNIVERSAL=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
TAURI_CONF="$REPO/apps/desktop/src-tauri/tauri.conf.json"

exec 3>&1 1>&2 # everything below logs to stderr; fd 3 is the real stdout for the app path

# `tauri build` exports MACOSX_DEPLOYMENT_TARGET from minimumSystemVersion. Building `cmm` with the
# same value makes it declare the same minimum macOS as the app.
MACOSX_DEPLOYMENT_TARGET="$(plutil -extract bundle.macOS.minimumSystemVersion raw -o - "$TAURI_CONF")" ||
  die "no bundle.macOS.minimumSystemVersion in $TAURI_CONF"
export MACOSX_DEPLOYMENT_TARGET
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0

cd "$REPO"
if [ "$UNIVERSAL" = 1 ]; then
  for triple in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target list --installed 2>/dev/null | grep -qx "$triple" ||
      die "Rust target $triple is missing: rustup target add aarch64-apple-darwin x86_64-apple-darwin"
  done
  echo "Building cmm (arm64 + x86_64)…"
  cargo build --release --locked -p cmm-cli --target aarch64-apple-darwin --target x86_64-apple-darwin
  OUT="$TARGET_DIR/universal-apple-darwin/release"
  mkdir -p "$OUT"
  lipo -create -output "$OUT/cmm" \
    "$TARGET_DIR/aarch64-apple-darwin/release/cmm" "$TARGET_DIR/x86_64-apple-darwin/release/cmm"
  TAURI_ARGS=(--target universal-apple-darwin)
else
  echo "Building cmm…"
  cargo build --release --locked -p cmm-cli
  OUT="$TARGET_DIR/release"
  TAURI_ARGS=()
fi

echo "Building ${APP_NAME}…"
corepack pnpm --filter desktop exec tauri build --bundles app ${TAURI_ARGS[@]+"${TAURI_ARGS[@]}"} -- --locked

APP="$OUT/bundle/macos/$APP_NAME"
[ -x "$APP/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")" ] ||
  die "tauri build did not produce $APP"

# One artifact for both: the CLI ships inside the app. The daily background scan (LaunchAgent) and
# the Homebrew cask's `binary` stanza both use this copy.
install -m 0755 "$OUT/cmm" "$APP/Contents/MacOS/cmm"
"$REPO/scripts/sign-app.sh" "$APP" -

echo "Built $APP ($("$APP/Contents/MacOS/cmm" --version), $(lipo -archs "$APP/Contents/MacOS/cmm"))"
printf '%s\n' "$APP" >&3
