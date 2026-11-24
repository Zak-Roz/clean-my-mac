#!/bin/bash
# Uninstalls clean-my-mac: removes the app bundle and the `cmm` CLI, nothing else.
#
#   scripts/uninstall.sh [--prefix DIR] [--app-dir DIR] [--bin-dir DIR] [--purge]
#
#   --prefix DIR    uninstall from DIR/Applications and DIR/bin (matches install.sh --prefix)
#   --app-dir DIR   where the .app is (default /Applications)
#   --bin-dir DIR   where `cmm` is (default ~/.local/bin)
#   --purge         also permanently delete the data (history/audit log, last report, config,
#                   rules.d) and the app's WebKit/cache folders; asks for confirmation first
#
# Without --purge, ~/Library/Application Support/clean-my-mac is never touched. Only things that
# are clean-my-mac's own (bundle id / CLI marker) are removed. Works with the macOS /bin/bash (3.2).
set -euo pipefail

APP_NAME="clean-my-mac.app"
BUNDLE_ID="com.zak-roz.clean-my-mac"
CLI_MARKER="Rule-based macOS disk cleaner" # clap `about` of cmm, embedded in the binary

APP_DIR="/Applications"
BIN_DIR="$HOME/.local/bin"
PURGE=0

die() { printf 'uninstall: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
usage() { sed -n '2,11s/^# \{0,1\}//p' "$0"; }

need_value() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a directory"; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix) need_value "$@"; APP_DIR="$2/Applications"; BIN_DIR="$2/bin"; shift 2 ;;
    --app-dir) need_value "$@"; APP_DIR="$2"; shift 2 ;;
    --bin-dir) need_value "$@"; BIN_DIR="$2"; shift 2 ;;
    --purge) PURGE=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done
APP_DIR="${APP_DIR%/}"
BIN_DIR="${BIN_DIR%/}"
[ -n "$HOME" ] && [ "$HOME" != "/" ] || die "HOME is not set to a home directory"

bundle_id_of() { /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null || true; }
is_our_cli() { LC_ALL=C grep -qa "$CLI_MARKER" "$1" 2>/dev/null; }

DEST_APP="$APP_DIR/$APP_NAME"
DEST_CLI="$BIN_DIR/cmm"

# --- decide everything before removing anything ---------------------------------------------------
REMOVE_APP=0
if [ -e "$DEST_APP" ] || [ -L "$DEST_APP" ]; then
  if [ -L "$DEST_APP" ] || [ ! -d "$DEST_APP" ]; then
    die "$DEST_APP is not an app bundle; not touching it"
  fi
  found_id="$(bundle_id_of "$DEST_APP")"
  [ "$found_id" = "$BUNDLE_ID" ] || die "$DEST_APP belongs to another app (${found_id:-no bundle id}); not removing it"
  pgrep -f "^$DEST_APP/Contents/MacOS/" >/dev/null 2>&1 && die "clean-my-mac is running from $DEST_APP — quit it first"
  REMOVE_APP=1
fi

REMOVE_CLI=0
if [ -e "$DEST_CLI" ] || [ -L "$DEST_CLI" ]; then
  is_our_cli "$DEST_CLI" || die "$DEST_CLI is not clean-my-mac's cmm; not removing it"
  REMOVE_CLI=1
fi

PURGE_PATHS=()
if [ "$PURGE" = 1 ]; then
  for p in \
    "$HOME/Library/Application Support/clean-my-mac" \
    "$HOME/Library/WebKit/$BUNDLE_ID" \
    "$HOME/Library/Caches/$BUNDLE_ID" \
    "$HOME/Library/Saved Application State/$BUNDLE_ID.savedState"; do
    if [ -L "$p" ]; then
      die "$p is a symlink; not following it — remove it by hand"
    elif [ -e "$p" ]; then
      PURGE_PATHS+=("$p")
    fi
  done
  # Any running copy (installed or a dev build) keeps the history database open.
  if [ "${#PURGE_PATHS[@]}" -gt 0 ] && pgrep -f "/$APP_NAME/Contents/MacOS/" >/dev/null 2>&1; then
    die "clean-my-mac is running — quit it before purging its data"
  fi
fi

if [ "$REMOVE_APP" = 0 ] && [ "$REMOVE_CLI" = 0 ] && [ "${#PURGE_PATHS[@]}" = 0 ]; then
  what="no $DEST_APP, no $DEST_CLI"
  [ "$PURGE" = 1 ] && what="$what, no clean-my-mac data"
  say "Nothing to remove: $what."
  exit 0
fi

if [ "${#PURGE_PATHS[@]}" -gt 0 ]; then
  # Ask on the terminal itself (not stdin), so `yes | uninstall.sh --purge` cannot confirm it.
  (exec </dev/tty) 2>/dev/null || die "--purge asks for confirmation on a terminal; none available, nothing removed"
  exec 3</dev/tty
  {
    say "--purge will PERMANENTLY delete (not move to the Trash):"
    for p in "${PURGE_PATHS[@]}"; do say "  $p ($(du -sh "$p" 2>/dev/null | awk '{print $1}'))"; done
    say "This includes the history database: the growth trend and the audit log of past cleans."
    printf "Type 'purge' to confirm: "
  } >/dev/tty
  answer=""
  read -r answer <&3 || true
  exec 3<&-
  [ "$answer" = "purge" ] || die "not confirmed; nothing removed"
fi

# --- remove --------------------------------------------------------------------------------------
if [ "$REMOVE_APP" = 1 ]; then
  rm -rf "$DEST_APP"
  say "Removed $DEST_APP"
fi
if [ "$REMOVE_CLI" = 1 ]; then
  rm -f "$DEST_CLI"
  say "Removed $DEST_CLI"
fi
for p in ${PURGE_PATHS[@]+"${PURGE_PATHS[@]}"}; do
  rm -rf "$p"
  say "Purged $p"
done
if [ "$PURGE" = 0 ] && [ -d "$HOME/Library/Application Support/clean-my-mac" ]; then
  say "Kept your data in $HOME/Library/Application Support/clean-my-mac (use --purge to delete it)."
fi
say "Full Disk Access entries are not removed automatically: System Settings → Privacy & Security."
