#!/bin/bash
# Checks that every place that carries the app version says <version>:
#   Cargo.toml ([workspace.package]), Cargo.lock (the workspace crates),
#   apps/desktop/src-tauri/tauri.conf.json and every package.json with a "version".
#
#   scripts/check-version.sh <version>     exits 1 and lists the mismatches
#   scripts/check-version.sh --print       prints the version in Cargo.toml
#
# Used by the release workflow (the tag must match) and by scripts/release.sh. Needs macOS (plutil).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

cargo_version() {
  awk -F'"' '/^\[/ { in_section = ($0 == "[workspace.package]") } in_section && /^version *=/ { print $2; exit }' Cargo.toml
}

case "${1:-}" in
  "" | -h | --help) sed -n '2,8s/^# \{0,1\}//p' "$0"; [ -n "${1:-}" ]; exit ;;
  --print) cargo_version; exit ;;
esac
EXPECTED="$1"

FAILED=0
check() { # <file> <found>
  if [ "$2" = "$EXPECTED" ]; then
    printf '  ok  %-45s %s\n' "$1" "$2"
  else
    printf '  !!  %-45s %s (expected %s)\n' "$1" "${2:-<none>}" "$EXPECTED"
    FAILED=1
  fi
}

echo "Version $EXPECTED:"
check "Cargo.toml" "$(cargo_version)"
for crate in cmm-core cmm-cli cmm-desktop; do
  check "Cargo.lock ($crate)" \
    "$(awk -F'"' -v name="$crate" '$0 == "name = \"" name "\"" { found = 1; next } found && /^version = / { print $2; exit }' Cargo.lock)"
done
check "apps/desktop/src-tauri/tauri.conf.json" \
  "$(plutil -extract version raw -o - apps/desktop/src-tauri/tauri.conf.json 2>/dev/null || true)"
while IFS= read -r manifest; do
  if found="$(plutil -extract version raw -o - "$manifest" 2>/dev/null)"; then
    check "$manifest" "$found"
  fi
done < <(git ls-files -- 'package.json' '*/package.json')
exit "$FAILED"
