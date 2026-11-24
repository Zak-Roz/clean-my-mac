#!/bin/bash
# Installs clean-my-mac: the app bundle into /Applications and the `cmm` CLI into ~/.local/bin.
#
#   scripts/install.sh [--build] [--prefix DIR] [--app-dir DIR] [--bin-dir DIR]
#
#   --build         build first: release `cmm` and `tauri build` (otherwise the existing
#                   target/release artifacts are installed)
#   --prefix DIR    install into DIR/Applications and DIR/bin instead (a trial run)
#   --app-dir DIR   where the .app goes (default /Applications)
#   --bin-dir DIR   where `cmm` goes (default ~/.local/bin, created if missing)
#
# An existing install is replaced only if it is clean-my-mac's own (bundle id / CLI marker) and
# the app is not running. Nothing in the data dir (~/Library/Application Support/clean-my-mac) is
# touched. Works with the macOS /bin/bash (3.2).
set -euo pipefail

APP_NAME="clean-my-mac.app"
BUNDLE_ID="com.zak-roz.clean-my-mac"
CLI_MARKER="Rule-based macOS disk cleaner" # clap `about` of cmm, embedded in the binary

REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
SRC_APP="$TARGET_DIR/release/bundle/macos/$APP_NAME"
SRC_CLI="$TARGET_DIR/release/cmm"

APP_DIR="/Applications"
BIN_DIR="$HOME/.local/bin"
BUILD=0

die() { printf 'install: %s\n' "$*" >&2; exit 1; }
warn() { printf 'install: warning: %s\n' "$*" >&2; }
say() { printf '%s\n' "$*"; }
usage() { sed -n '2,10s/^# \{0,1\}//p' "$0"; }

need_value() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a directory"; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --build) BUILD=1; shift ;;
    --prefix) need_value "$@"; APP_DIR="$2/Applications"; BIN_DIR="$2/bin"; shift 2 ;;
    --app-dir) need_value "$@"; APP_DIR="$2"; shift 2 ;;
    --bin-dir) need_value "$@"; BIN_DIR="$2"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done
APP_DIR="${APP_DIR%/}"
BIN_DIR="${BIN_DIR%/}"

bundle_id_of() { /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null || true; }
is_our_cli() { LC_ALL=C grep -qa "$CLI_MARKER" "$1" 2>/dev/null; }
app_running_from() { pgrep -f "^$1/Contents/MacOS/" >/dev/null 2>&1; }

if [ "$BUILD" = 1 ]; then
  say "Building the release CLI and app (this takes a few minutes)…"
  (cd "$REPO" && cargo build --release -p cmm-cli)
  (cd "$REPO" && COREPACK_ENABLE_DOWNLOAD_PROMPT=0 corepack pnpm --filter desktop tauri build)
fi

[ -d "$SRC_APP" ] || die "no app bundle at $SRC_APP — build it first (scripts/install.sh --build)"
[ -x "$SRC_CLI" ] || die "no CLI at $SRC_CLI — build it first (scripts/install.sh --build)"
[ "$(bundle_id_of "$SRC_APP")" = "$BUNDLE_ID" ] || die "$SRC_APP is not clean-my-mac ($BUNDLE_ID)"
is_our_cli "$SRC_CLI" || die "$SRC_CLI is not clean-my-mac's cmm"

# --- check both destinations before changing anything ----------------------------------------------
DEST_APP="$APP_DIR/$APP_NAME"
DEST_CLI="$BIN_DIR/cmm"
if [ -e "$DEST_APP" ] || [ -L "$DEST_APP" ]; then
  if [ -L "$DEST_APP" ] || [ ! -d "$DEST_APP" ]; then
    die "$DEST_APP exists but is not an app bundle; not touching it"
  fi
  found_id="$(bundle_id_of "$DEST_APP")"
  [ "$found_id" = "$BUNDLE_ID" ] || die "$DEST_APP belongs to another app (${found_id:-no bundle id}); not replacing it"
  app_running_from "$DEST_APP" && die "clean-my-mac is running from $DEST_APP — quit it first"
fi
if [ -e "$DEST_CLI" ] || [ -L "$DEST_CLI" ]; then
  is_our_cli "$DEST_CLI" || die "$DEST_CLI exists and is not clean-my-mac's cmm; not replacing it"
fi
mkdir -p "$APP_DIR" || die "cannot create $APP_DIR"
[ -w "$APP_DIR" ] || die "$APP_DIR is not writable by $(id -un)"
mkdir -p "$BIN_DIR" || die "cannot create $BIN_DIR"
[ -w "$BIN_DIR" ] || die "$BIN_DIR is not writable by $(id -un)"

# --- app -----------------------------------------------------------------------------------------
# Copy next to the destination first, then swap, so a failed copy never leaves a half-written app.
STAGE="$(mktemp -d "$APP_DIR/.clean-my-mac-install.XXXXXX")"
cleanup() { if [ -n "${STAGE:-}" ] && [ -d "$STAGE" ]; then rm -rf "$STAGE"; fi; }
trap cleanup EXIT
ditto "$SRC_APP" "$STAGE/$APP_NAME" # keeps the code signature, xattrs and symlinks intact
[ -d "$DEST_APP" ] && mv "$DEST_APP" "$STAGE/previous.app"
mv "$STAGE/$APP_NAME" "$DEST_APP"
cleanup
trap - EXIT
say "Installed $DEST_APP"

# --- CLI -----------------------------------------------------------------------------------------
# New file + rename (not an in-place overwrite): macOS caches code signatures per inode, and
# overwriting a running or recently run signed binary in place gets it killed at launch.
install -m 0755 "$SRC_CLI" "$DEST_CLI.tmp.$$"
mv -f "$DEST_CLI.tmp.$$" "$DEST_CLI"
say "Installed $DEST_CLI ($("$DEST_CLI" --version))"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR is not on your PATH. Add it with:
    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc && exec zsh" ;;
esac

cat <<EOF

Next: give clean-my-mac Full Disk Access (System Settings → Privacy & Security → Full Disk Access
→ + → $DEST_APP). Without it the Trash size is unknown and protected caches are skipped.
This build is ad-hoc signed: after every reinstall, remove the old entry (−) and add the app again.
For \`cmm\` in a terminal, give Full Disk Access to your terminal app instead.
EOF
