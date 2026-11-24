#!/bin/bash
# Prepares a release: sets the version everywhere, commits and tags. Pushing is left to you.
#
#   scripts/release.sh <version> [--allow-branch]
#
#   <version>        1.2.3, or 1.2.3-beta.1 for a pre-release (Homebrew skips those)
#   --allow-branch   release from a branch other than main
#
# Updates Cargo.toml ([workspace.package]), Cargo.lock, apps/desktop/src-tauri/tauri.conf.json and
# every package.json with a "version", checks them with scripts/check-version.sh, commits
# "release: v<version>" and makes the annotated tag v<version>. Pushing the tag starts
# .github/workflows/release.yml. Works with the macOS /bin/bash (3.2).
set -euo pipefail

die() { printf 'release: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,12s/^# \{0,1\}//p' "$0"; }

VERSION=""
ALLOW_BRANCH=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --allow-branch) ALLOW_BRANCH=1; shift ;;
    -h | --help) usage; exit 0 ;;
    -*) usage >&2; die "unknown argument: $1" ;;
    *) [ -z "$VERSION" ] || die "one version only"; VERSION="${1#v}"; shift ;;
  esac
done
[ -n "$VERSION" ] || { usage >&2; exit 1; }
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || die "'$VERSION' is not a version like 1.2.3 or 1.2.3-beta.1"
TAG="v$VERSION"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BRANCH="$(git symbolic-ref --quiet --short HEAD)" || die "HEAD is detached; check out main first"
[ "$BRANCH" = main ] || [ "$ALLOW_BRANCH" = 1 ] || die "on branch $BRANCH; release from main (or pass --allow-branch)"
[ -z "$(git status --porcelain)" ] || die "the working tree has changes; commit or stash them first"
git rev-parse --quiet --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG exists already"
if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
  die "tag $TAG exists on origin already"
fi

CURRENT="$(scripts/check-version.sh --print)"
[ "$CURRENT" != "$VERSION" ] || die "the version is $VERSION already"
NEWEST="$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sed 's/-/~/' | sort -V | tail -1 | sed 's/~/-/')"
[ "$NEWEST" = "$VERSION" ] || die "$VERSION is older than the current version $CURRENT"

# Cargo.toml: the version line of [workspace.package] only.
awk -v v="$VERSION" '
  /^\[/ { in_section = ($0 == "[workspace.package]") }
  in_section && /^version *=/ && !done { print "version = \"" v "\""; done = 1; next }
  { print }
' Cargo.toml >Cargo.toml.new
mv Cargo.toml.new Cargo.toml

# JSON manifests: the top-level "version" line, edited in place so the formatting stays.
set_json_version() { # <file>
  local old
  old="$(plutil -extract version raw -o - "$1")"
  sed -i '' "s/^  \"version\": \"$(printf '%s' "$old" | sed 's/\./\\./g')\",\$/  \"version\": \"$VERSION\",/" "$1"
}
MANIFESTS=(apps/desktop/src-tauri/tauri.conf.json)
while IFS= read -r manifest; do
  if plutil -extract version raw -o - "$manifest" >/dev/null 2>&1; then MANIFESTS+=("$manifest"); fi
done < <(git ls-files -- 'package.json' '*/package.json')
for manifest in "${MANIFESTS[@]}"; do set_json_version "$manifest"; done

# Cargo.lock: only the workspace crates' versions change; no dependency is updated.
cargo update --workspace --offline --quiet

scripts/check-version.sh "$VERSION"

git add Cargo.toml Cargo.lock "${MANIFESTS[@]}"
git commit --quiet -m "release: $TAG"
git tag -a "$TAG" -m "clean-my-mac $VERSION"

cat <<EOF

Committed "release: $TAG" and tagged $TAG on $BRANCH. Nothing is pushed. To publish:

  git push --atomic origin $BRANCH $TAG

The Release workflow then builds, signs and publishes clean-my-mac.zip and updates the Homebrew cask.
EOF
