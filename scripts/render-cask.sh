#!/bin/bash
# Renders the Homebrew cask (packaging/homebrew/clean-my-mac.rb.tmpl) to stdout.
#
#   scripts/render-cask.sh --version 0.2.0 --sha256 <sha256 of clean-my-mac.zip> [--repo OWNER/NAME]
#
# The bundle identifier (and the LaunchAgent label <id>.scan) and the minimum macOS come from
# apps/desktop/src-tauri/tauri.conf.json, so the cask always matches the app. --repo defaults to
# $GITHUB_REPOSITORY, then to the GitHub repository of the `origin` remote. The tap is
# <owner>/homebrew-tap, written as <owner>/tap in brew commands. Needs macOS (plutil).
set -euo pipefail

die() { printf 'render-cask: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,9s/^# \{0,1\}//p' "$0"; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$ROOT/packaging/homebrew/clean-my-mac.rb.tmpl"
TAURI_CONF="$ROOT/apps/desktop/src-tauri/tauri.conf.json"

VERSION=""
SHA256=""
REPO="${GITHUB_REPOSITORY:-}"
need_value() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a value"; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) need_value "$@"; VERSION="$2"; shift 2 ;;
    --sha256) need_value "$@"; SHA256="$2"; shift 2 ;;
    --repo) need_value "$@"; REPO="$2"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

if [ -z "$REPO" ]; then
  url="$(git -C "$ROOT" remote get-url origin 2>/dev/null || true)"
  REPO="$(printf '%s\n' "$url" | sed -nE 's#^(https://([^/@]+@)?github\.com/|git@github\.com:|ssh://git@github\.com/)([^/]+/[^/]+)$#\3#p')"
  REPO="${REPO%.git}"
fi

# Everything substituted is validated first, so nothing can break out of a Ruby string.
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--version must look like 1.2.3 (got '$VERSION')"
[[ "$SHA256" =~ ^[0-9a-f]{64}$ ]] || die "--sha256 must be 64 lowercase hex digits"
[[ "$REPO" =~ ^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$ ]] || die "--repo must look like OWNER/NAME (got '$REPO')"

BUNDLE_ID="$(plutil -extract identifier raw -o - "$TAURI_CONF")"
[[ "$BUNDLE_ID" =~ ^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$ ]] || die "unexpected bundle identifier '$BUNDLE_ID'"

MIN_MACOS="$(plutil -extract bundle.macOS.minimumSystemVersion raw -o - "$TAURI_CONF")"
case "${MIN_MACOS%%.*}" in
  12) MACOS=monterey ;;
  13) MACOS=ventura ;;
  14) MACOS=sonoma ;;
  15) MACOS=sequoia ;;
  26) MACOS=tahoe ;;
  27) MACOS=golden_gate ;;
  *) die "no Homebrew name for macOS $MIN_MACOS: add it to $0" ;;
esac
case "$MIN_MACOS" in
  *.*.*) die "minimumSystemVersion $MIN_MACOS: a cask can only require a major macOS version" ;;
  *.[1-9]*) die "minimumSystemVersion $MIN_MACOS: a cask can only require a major macOS version" ;;
esac

OWNER="${REPO%%/*}"
TAP="$(printf '%s' "$OWNER" | tr '[:upper:]' '[:lower:]')/tap"

RENDERED="$(sed \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@SHA256@|$SHA256|g" \
  -e "s|@REPO@|$REPO|g" \
  -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
  -e "s|@MACOS@|$MACOS|g" \
  -e "s|@TAP@|$TAP|g" \
  "$TEMPLATE")"
if grep -qE '@[A-Z_]+@' <<<"$RENDERED"; then
  die "unreplaced placeholders: $(grep -oE '@[A-Z_]+@' <<<"$RENDERED" | sort -u | tr '\n' ' ')"
fi
printf '%s\n' "$RENDERED"
