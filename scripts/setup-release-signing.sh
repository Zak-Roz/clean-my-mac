#!/bin/bash
# One-time setup of release signing and Homebrew publishing. The repository owner runs it once,
# on their own Mac, before the first release:
#
#   scripts/setup-release-signing.sh [--dry-run] [--reuse-cert | --force]
#                                    [--repo OWNER/NAME] [--tap OWNER/NAME] [--backup-dir DIR]
#
# It
#   1. creates the self-signed code signing certificate "clean-my-mac Release" (RSA 3072, code
#      signing only, valid 20 years) as a .p12 with a random password;
#   2. keeps the .p12, its password and the certificate in a private folder outside the repository
#      (default ~/Library/Application Support/clean-my-mac-release-signing; folder 700, files 600).
#      Back it up: every release must be signed with this certificate, or users have to grant
#      Full Disk Access again;
#   3. sets the MACOS_CERT_P12 and MACOS_CERT_PASSWORD secrets on the app repository;
#   4. creates an ed25519 key pair, adds the public half as a write deploy key on the tap and sets
#      the private half as the HOMEBREW_TAP_DEPLOY_KEY secret (it is kept nowhere else; rerunning
#      the script makes a new one). An existing deploy key with its secret is kept.
#
#   --dry-run      check everything and print the plan; change nothing
#   --reuse-cert   upload the certificate from the backup folder again instead of creating one
#                  (for example after a run that failed half way)
#   --force        create a new certificate even though one exists, and replace the deploy key.
#                  Users must then grant Full Disk Access again. The old backup is moved aside,
#                  never deleted
#   --repo         the app repository (default: the GitHub repository of the `origin` remote)
#   --tap          the Homebrew tap (default: <owner>/homebrew-tap)
#   --backup-dir   the backup folder (see 2.)
#
# Needs gh with admin rights on both repositories. If the active gh account is another one, the
# repository owner's account is used when it is logged in too (gh auth token -u <owner>).
# Nothing secret is printed. Works with the macOS /bin/bash (3.2).
set -euo pipefail

CERT_NAME="clean-my-mac Release"
CERT_DAYS=7305 # 20 years
KEY_TITLE="clean-my-mac release workflow"
# LibreSSL, as shipped with macOS: its .p12 files import with `security` (OpenSSL 3's default
# encryption does not), and the release workflow imports the .p12 that way.
OPENSSL=/usr/bin/openssl

die() { printf 'setup-release-signing: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
usage() { sed -n '2,31s/^# \{0,1\}//p' "$0"; }

DRY_RUN=0
REUSE=0
FORCE=0
REPO=""
TAP=""
BACKUP_DIR="$HOME/Library/Application Support/clean-my-mac-release-signing"
need_value() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a value"; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --reuse-cert) REUSE=1; shift ;;
    --force) FORCE=1; shift ;;
    --repo) need_value "$@"; REPO="$2"; shift 2 ;;
    --tap) need_value "$@"; TAP="$2"; shift 2 ;;
    --backup-dir) need_value "$@"; BACKUP_DIR="${2%/}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done
[ "$REUSE" = 1 ] && [ "$FORCE" = 1 ] && die "--reuse-cert and --force exclude each other"
[ "$(uname -s)" = Darwin ] || die "run this on a Mac"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ -z "$REPO" ]; then
  url="$(git -C "$ROOT" remote get-url origin 2>/dev/null || true)"
  REPO="$(printf '%s\n' "$url" | sed -nE 's#^(https://([^/@]+@)?github\.com/|git@github\.com:|ssh://git@github\.com/)([^/]+/[^/]+)$#\3#p')"
  REPO="${REPO%.git}"
fi
[[ "$REPO" =~ ^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$ ]] || die "--repo must look like OWNER/NAME (got '$REPO')"
OWNER="${REPO%%/*}"
TAP="${TAP:-$OWNER/homebrew-tap}"
[[ "$TAP" =~ ^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$ ]] || die "--tap must look like OWNER/NAME (got '$TAP')"
case "$BACKUP_DIR" in
  /*) ;;
  *) die "--backup-dir must be an absolute path" ;;
esac
case "$BACKUP_DIR/" in
  "$ROOT"/*) die "the backup folder must be outside the repository" ;;
esac

P12="$BACKUP_DIR/clean-my-mac-release.p12"
PASSWORD_FILE="$BACKUP_DIR/clean-my-mac-release.p12.password"
CERT_PEM="$BACKUP_DIR/clean-my-mac-release.pem"

for tool in gh git ssh-keygen base64 security; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is missing"
done
[ -x "$OPENSSL" ] || die "$OPENSSL is missing"

# --- gh account and rights ----------------------------------------------------------------------
if [ -z "${GH_TOKEN:-}" ] && OWNER_TOKEN="$(gh auth token -u "$OWNER" 2>/dev/null)" && [ -n "$OWNER_TOKEN" ]; then
  GH_TOKEN="$OWNER_TOKEN"
  export GH_TOKEN
fi
unset OWNER_TOKEN
LOGIN="$(gh api user --jq .login 2>/dev/null)" || die "gh is not logged in (gh auth login)"
for r in "$REPO" "$TAP"; do
  admin="$(gh api "repos/$r" --jq .permissions.admin 2>/dev/null)" || die "$LOGIN cannot see $r"
  [ "$admin" = true ] || die "$LOGIN is not an admin of $r (secrets and deploy keys need admin)"
done

# --- what exists already --------------------------------------------------------------------------
SECRETS="$(gh secret list --repo "$REPO" --json name --jq '.[].name')"
has_secret() { grep -qx "$1" <<<"$SECRETS"; }
OLD_KEY_IDS="$(gh repo deploy-key list --repo "$TAP" --json id,title --jq ".[] | select(.title == \"$KEY_TITLE\") | .id")"
OLD_KEY_IDS="$(printf '%s' "$OLD_KEY_IDS" | tr '\n' ' ')"
BACKUP_EXISTS=0
if [ -e "$P12" ] || [ -e "$PASSWORD_FILE" ]; then BACKUP_EXISTS=1; fi
KEYCHAIN_CERT=0
if security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; then KEYCHAIN_CERT=1; fi

CONFLICTS=()
if [ "$REUSE" = 1 ]; then
  [ -f "$P12" ] && [ -f "$PASSWORD_FILE" ] || die "--reuse-cert: no .p12 and password in $BACKUP_DIR"
  CERT_ACTION=reuse
else
  if [ "$FORCE" = 0 ]; then
    [ "$BACKUP_EXISTS" = 1 ] && CONFLICTS+=("a certificate backup in $BACKUP_DIR (upload it again: --reuse-cert)")
    [ "$KEYCHAIN_CERT" = 1 ] && CONFLICTS+=("a \"$CERT_NAME\" certificate in your keychains (to release with it, export it with its key as $P12, its password in $PASSWORD_FILE, then use --reuse-cert)")
    has_secret MACOS_CERT_P12 && CONFLICTS+=("the MACOS_CERT_P12 secret on $REPO")
    has_secret MACOS_CERT_PASSWORD && CONFLICTS+=("the MACOS_CERT_PASSWORD secret on $REPO")
  fi
  CERT_ACTION=create
fi
if [ -n "$OLD_KEY_IDS" ] && has_secret HOMEBREW_TAP_DEPLOY_KEY && [ "$FORCE" = 0 ]; then
  KEY_ACTION=keep
else
  KEY_ACTION=create
fi

# --- plan -----------------------------------------------------------------------------------------
say "App repository:  $REPO (gh account $LOGIN, admin)"
say "Homebrew tap:    $TAP"
say "Backup folder:   $BACKUP_DIR"
say ""
say "Plan:"
if [ "$CERT_ACTION" = create ]; then
  say "  1. create the certificate \"$CERT_NAME\" (RSA 3072, code signing, valid until $(date -v+"${CERT_DAYS}"d +%Y-%m-%d)) and a random .p12 password"
  if [ "$BACKUP_EXISTS" = 1 ]; then
    say "  2. move the current backup aside to $BACKUP_DIR/replaced-<date>/, then save the new .p12, password and certificate there"
  else
    say "  2. save the .p12, its password and the certificate in the backup folder (folder 700, files 600)"
  fi
else
  say "  1. use the certificate in the backup folder: $(
    "$OPENSSL" pkcs12 -in "$P12" -passin "file:$PASSWORD_FILE" -nokeys -clcerts 2>/dev/null |
      "$OPENSSL" x509 -noout -subject -enddate 2>/dev/null | tr '\n' ' '
  )"
  say "  2. (the backup stays as it is)"
fi
say "  3. set the secrets MACOS_CERT_P12 and MACOS_CERT_PASSWORD on $REPO"
if [ "$KEY_ACTION" = keep ]; then
  say "  4. keep the deploy key \"$KEY_TITLE\" on $TAP and the HOMEBREW_TAP_DEPLOY_KEY secret"
else
  say "  4. create an ed25519 deploy key, add it to $TAP as \"$KEY_TITLE\" with write access,"
  say "     set the HOMEBREW_TAP_DEPLOY_KEY secret on $REPO${OLD_KEY_IDS:+, then remove the old deploy key(s) $OLD_KEY_IDS}"
fi

if [ "${#CONFLICTS[@]}" -gt 0 ]; then
  say ""
  say "Stopping: a signing certificate exists already. Every release must be signed with the same"
  say "certificate, or users have to grant Full Disk Access again. Found:"
  for c in "${CONFLICTS[@]}"; do say "  - $c"; done
  say "Replace it anyway with --force."
  exit 1
fi
if [ "$DRY_RUN" = 1 ]; then
  say ""
  say "Dry run: nothing was changed."
  exit 0
fi

# --- 1 + 2: certificate ---------------------------------------------------------------------------
umask 077
WORK="$(mktemp -d "${TMPDIR:-/tmp}/clean-my-mac-signing.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [ "$CERT_ACTION" = create ]; then
  step "Creating the certificate \"$CERT_NAME\""
  cat >"$WORK/cert.cnf" <<EOF
[ req ]
distinguished_name = dn
x509_extensions = ext
prompt = no
[ dn ]
CN = $CERT_NAME
[ ext ]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
EOF
  "$OPENSSL" rand -hex 24 | tr -d '\n' >"$WORK/password"
  "$OPENSSL" req -x509 -new -newkey rsa:3072 -nodes -sha256 -days "$CERT_DAYS" -config "$WORK/cert.cnf" \
    -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>"$WORK/openssl.log" || { cat "$WORK/openssl.log" >&2; die "openssl req failed"; }
  "$OPENSSL" pkcs12 -export -name "$CERT_NAME" -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
    -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 \
    -passout "file:$WORK/password" -out "$WORK/cert.p12" 2>"$WORK/openssl.log" || { cat "$WORK/openssl.log" >&2; die "openssl pkcs12 failed"; }
  rm -f "$WORK/key.pem"
  "$OPENSSL" x509 -in "$WORK/cert.pem" -noout -text | grep -q 'Code Signing' || die "the certificate lacks the code signing usage"
  "$OPENSSL" pkcs12 -in "$WORK/cert.p12" -passin "file:$WORK/password" -noout 2>/dev/null || die "cannot read back the .p12"

  step "Saving the backup in $BACKUP_DIR"
  if [ "$BACKUP_EXISTS" = 1 ]; then
    ASIDE="$BACKUP_DIR/replaced-$(date +%Y%m%d-%H%M%S)"
    mkdir -m 700 "$ASIDE"
    for f in "$P12" "$PASSWORD_FILE" "$CERT_PEM"; do
      if [ -e "$f" ]; then mv "$f" "$ASIDE/"; fi
    done
    say "Moved the previous certificate to $ASIDE"
  fi
  mkdir -p "$BACKUP_DIR"
  chmod 700 "$BACKUP_DIR"
  install -m 600 "$WORK/cert.p12" "$P12"
  install -m 600 "$WORK/password" "$PASSWORD_FILE"
  install -m 600 "$WORK/cert.pem" "$CERT_PEM"
  cat >"$BACKUP_DIR/README.txt" <<EOF
clean-my-mac release signing certificate ("$CERT_NAME").

$(basename "$P12")           the certificate and its private key
$(basename "$PASSWORD_FILE")  the .p12 password
$(basename "$CERT_PEM")           the certificate alone

The release workflow of $REPO signs every release with it (secrets MACOS_CERT_P12 and
MACOS_CERT_PASSWORD). Keep a copy somewhere safe: a release signed with another certificate makes
macOS forget the Full Disk Access users granted. To upload it again:
  scripts/setup-release-signing.sh --reuse-cert
EOF
  chmod 600 "$BACKUP_DIR/README.txt"
fi

# --- 3: certificate secrets -----------------------------------------------------------------------
step "Setting MACOS_CERT_P12 and MACOS_CERT_PASSWORD on $REPO"
base64 -i "$P12" | tr -d '\n' | gh secret set MACOS_CERT_P12 --repo "$REPO"
gh secret set MACOS_CERT_PASSWORD --repo "$REPO" <"$PASSWORD_FILE"

# --- 4: deploy key --------------------------------------------------------------------------------
if [ "$KEY_ACTION" = keep ]; then
  step "Keeping the deploy key \"$KEY_TITLE\" on $TAP"
else
  step "Adding a write deploy key to $TAP and setting HOMEBREW_TAP_DEPLOY_KEY"
  ssh-keygen -q -t ed25519 -N "" -C "$KEY_TITLE" -f "$WORK/tap_key"
  gh repo deploy-key add "$WORK/tap_key.pub" --repo "$TAP" --allow-write --title "$KEY_TITLE"
  gh secret set HOMEBREW_TAP_DEPLOY_KEY --repo "$REPO" <"$WORK/tap_key"
  for id in $OLD_KEY_IDS; do
    gh repo deploy-key delete "$id" --repo "$TAP"
  done
fi

cat <<EOF

Done. Back up $BACKUP_DIR (a password manager or an encrypted
disk image): without it the next certificate is a new one, and users must grant Full Disk
Access again.

First release: scripts/release.sh <version>, then run the git push it prints.
EOF
