#!/bin/bash
# Signs a built clean-my-mac.app inside out, with the hardened runtime on every executable.
#
#   scripts/sign-app.sh <path/to/clean-my-mac.app> <identity> [keychain]
#
# <identity> is a code signing identity (its SHA-1 hash or name), or "-" to sign ad hoc (local
# builds). Extra executables in Contents/MacOS (the `cmm` CLI) are signed first, each with the
# identifier <bundle id>.<name>, then the app itself.
#
# Signed with a certificate, every piece gets the designated requirement
#   identifier "<id>" and certificate leaf = H"<certificate hash>"
# which stays the same from one release to the next, so macOS keeps the Full Disk Access the user
# granted. An ad-hoc signature's requirement is its cdhash, which changes with every build.
#
# Why not `codesign --deep`: it signs nested code in no particular order and gives the nested
# CLI the app's identifier.
set -euo pipefail

USAGE="usage: sign-app.sh <clean-my-mac.app> <identity> [keychain]"
APP="${1:?$USAGE}"
IDENTITY="${2:?$USAGE}"
KEYCHAIN="${3:-}"
APP="${APP%/}"

PLIST="$APP/Contents/Info.plist"
[ -f "$PLIST" ] || { echo "sign-app: $APP is not an app bundle" >&2; exit 1; }
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PLIST")"
MAIN="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST")"

SIGN=(codesign --force --options runtime --timestamp=none --sign "$IDENTITY")
if [ -n "$KEYCHAIN" ]; then
  SIGN+=(--keychain "$KEYCHAIN")
fi

CODE=()
for EXE in "$APP"/Contents/MacOS/*; do
  [ "$(basename "$EXE")" = "$MAIN" ] && continue
  "${SIGN[@]}" --identifier "$BUNDLE_ID.$(basename "$EXE")" "$EXE"
  CODE+=("$EXE")
done
"${SIGN[@]}" --preserve-metadata=entitlements "$APP"
CODE+=("$APP")

codesign --verify --deep --strict --verbose=2 "$APP"

# Every piece must carry the hardened runtime and no get-task-allow; signed with a certificate,
# every designated requirement must name the certificate, not a cdhash. (Outputs are captured
# before grepping: `codesign … | grep -q` fails under pipefail when grep exits early.)
FAILED=0
for C in "${CODE[@]}"; do
  DETAILS="$(codesign -dvv "$C" 2>&1 || true)"
  ENTITLEMENTS="$(codesign -d --entitlements - "$C" 2>/dev/null || true)"
  REQUIREMENT="$(codesign -d -r- "$C" 2>&1 || true)"
  if ! grep -q 'flags=.*runtime' <<<"$DETAILS"; then
    echo "error: no hardened runtime on $C" >&2
    FAILED=1
  fi
  if grep -q 'get-task-allow' <<<"$ENTITLEMENTS"; then
    echo "error: get-task-allow entitlement on $C" >&2
    FAILED=1
  fi
  if [ "$IDENTITY" != "-" ] && { ! grep -q 'certificate leaf' <<<"$REQUIREMENT" || grep -q 'cdhash' <<<"$REQUIREMENT"; }; then
    echo "error: the designated requirement of $C is not tied to the certificate" >&2
    FAILED=1
  fi
  echo "$C: $(sed -n 's/^#* *designated => //p' <<<"$REQUIREMENT")"
done
exit "$FAILED"
