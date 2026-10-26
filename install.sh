#!/bin/sh
# farwork-agent installer.
#
# POSIX sh on purpose: this file is piped into `sh` on customer machines, where
# that shell is macOS's bash 3.2 in sh mode, dash on Debian/Ubuntu, or busybox
# ash. No bashisms, no Python, no Node: sh + curl/wget + tar + a SHA-256 tool are
# the whole dependency list. It never touches a package manager, never needs
# root, and never needs Rust or the source tree.
#
#   curl -fsSL https://github.com/vpertj/farwork/releases/latest/download/install.sh | sh
#
# Environment:
#   FARWORK_VERSION            release tag to install, e.g. v0.1.0 (default: latest)
#   FARWORK_INSTALL_DIR        destination directory (default: $HOME/.local/bin)
#   FARWORK_RELEASE_BASE_URL   fetch assets from here instead of GitHub releases
#                              (mirrors, and the local end-to-end test)
#
# Exit codes: 0 installed; 1 otherwise (unsupported platform, download failure,
# missing checksum entry, checksum mismatch, or a binary that does not run here).
# Idempotent: re-running re-downloads the same release and replaces the binary
# in one rename, so an interrupted run never leaves a half-written executable.

set -eu

BIN_NAME=farwork-agent
DEFAULT_RELEASE_URL=https://github.com/vpertj/farwork/releases
VERSION=${FARWORK_VERSION:-latest}

die() { printf 'install: error: %s\n' "$*" >&2; exit 1; }
info() { printf 'install: %s\n' "$*"; }

usage() {
  cat <<'EOF'
Install the farwork-agent prebuilt binary into ~/.local/bin.

    curl -fsSL https://github.com/vpertj/farwork/releases/latest/download/install.sh | sh

Environment:
    FARWORK_VERSION            release tag to install (default: latest)
    FARWORK_INSTALL_DIR        destination directory (default: $HOME/.local/bin)
    FARWORK_RELEASE_BASE_URL   fetch assets from here instead of GitHub releases
EOF
}

for arg in "$@"; do
  case "$arg" in
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $arg (try --help)" ;;
  esac
done

[ -n "${HOME:-}" ] || die "HOME is not set; cannot pick an install directory"

INSTALL_DIR=${FARWORK_INSTALL_DIR:-$HOME/.local/bin}

# --- platform -> release asset -------------------------------------------------
case "$(uname -s)" in
  Darwin) os_part=apple-darwin ;;
  Linux) os_part=unknown-linux-gnu ;;
  *) die "unsupported operating system: $(uname -s) (macOS and Linux have prebuilt binaries; on Windows use the .zip from the release page)" ;;
esac

case "$(uname -m)" in
  x86_64|amd64) arch_part=x86_64 ;;
  arm64|aarch64) arch_part=aarch64 ;;
  *) die "unsupported CPU architecture: $(uname -m) (prebuilt binaries cover x86_64 and arm64/aarch64)" ;;
esac

# The published Linux builds link glibc, so a musl-only distro (Alpine) cannot
# run them. Say so instead of installing a binary that dies with "not found".
if [ "$os_part" = unknown-linux-gnu ] && command -v ldd >/dev/null 2>&1; then
  if ldd --version 2>&1 | grep -qi musl; then
    die "this is a musl system (e.g. Alpine), but the published Linux binary needs glibc; build from source or run in a glibc image"
  fi
fi

TARGET="${arch_part}-${os_part}"
ASSET="${BIN_NAME}-${TARGET}.tar.gz"

if [ -n "${FARWORK_RELEASE_BASE_URL:-}" ]; then
  BASE_URL=${FARWORK_RELEASE_BASE_URL%/}
elif [ "$VERSION" = latest ]; then
  BASE_URL=$DEFAULT_RELEASE_URL/latest/download
else
  BASE_URL=$DEFAULT_RELEASE_URL/download/$VERSION
fi

info "target:  $TARGET"
info "release: $BASE_URL"

# --- tooling ------------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 --retry-delay 1 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "neither curl nor wget is installed; install one of them and retry"
fi

command -v tar >/dev/null 2>&1 || die "tar is not installed; cannot unpack $ASSET"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/.*[[:space:]]//'
  else
    die "no SHA-256 tool found (need sha256sum, shasum or openssl)"
  fi
}

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/farwork-install.XXXXXX") \
  || die "cannot create a temporary directory under ${TMPDIR:-/tmp}"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT HUP INT TERM

# --- download + verify --------------------------------------------------------
info "downloading $ASSET"
fetch "$BASE_URL/$ASSET" "$TMP_DIR/$ASSET" \
  || die "cannot download $BASE_URL/$ASSET (check the version/tag and your network)"
fetch "$BASE_URL/SHA256SUMS" "$TMP_DIR/SHA256SUMS" \
  || die "cannot download $BASE_URL/SHA256SUMS (the release is incomplete; refusing to install)"

EXPECTED=$(awk -v name="$ASSET" '
  { file = $2; sub(/^\*/, "", file); if (file == name) { print $1; exit } }
' "$TMP_DIR/SHA256SUMS")
[ -n "$EXPECTED" ] || die "SHA256SUMS has no entry for $ASSET; refusing to install"

ACTUAL=$(sha256_of "$TMP_DIR/$ASSET")
EXPECTED_LC=$(printf '%s' "$EXPECTED" | tr 'ABCDEF' 'abcdef')
ACTUAL_LC=$(printf '%s' "$ACTUAL" | tr 'ABCDEF' 'abcdef')

if [ "$ACTUAL_LC" != "$EXPECTED_LC" ]; then
  printf 'install: error: SHA-256 mismatch for %s\n' "$ASSET" >&2
  printf 'install:   expected: %s\n' "$EXPECTED_LC" >&2
  printf 'install:   actual:   %s\n' "$ACTUAL_LC" >&2
  die "refusing to install a corrupted or tampered download"
fi
info "sha256 ok: $ACTUAL_LC"

# --- unpack + probe + install --------------------------------------------------
tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR" \
  || die "cannot unpack $ASSET (corrupt archive?)"
STAGED="$TMP_DIR/$BIN_NAME"
[ -f "$STAGED" ] || die "archive $ASSET does not contain $BIN_NAME"
chmod 755 "$STAGED"

# Running the binary here is the only honest proof that it works on this
# machine (wrong glibc, wrong architecture, or a broken build all show up now
# rather than in the middle of a session).
INSTALLED_VERSION=$("$STAGED" --version 2>&1) \
  || die "the downloaded $BIN_NAME does not run on this machine: $INSTALLED_VERSION"

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
# Copy next to the destination, then rename: a rename is atomic, so a crash
# mid-install cannot leave a truncated farwork-agent on PATH.
STAGED_DEST="$INSTALL_DIR/.${BIN_NAME}.new.$$"
cp "$STAGED" "$STAGED_DEST" || die "cannot write into $INSTALL_DIR"
chmod 755 "$STAGED_DEST"
mv -f "$STAGED_DEST" "$INSTALL_DIR/$BIN_NAME" || die "cannot replace $INSTALL_DIR/$BIN_NAME"

info "installed $INSTALLED_VERSION -> $INSTALL_DIR/$BIN_NAME"

case ":${PATH:-}:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\n%s is not on PATH in this shell. Add it once:\n\n' "$INSTALL_DIR" >&2
    # shellcheck disable=SC2016  # $PATH must reach the reader's shell unexpanded
    printf '    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" >&2
    printf '  (append that line to ~/.profile or ~/.zshrc to keep it)\n\n' >&2
    ;;
esac

printf 'Next: ask your hub for a connector token, then run\n\n'
printf '    %s --hub wss://<your-server> --token '"'"'<connectorToken>'"'"'\n\n' "$BIN_NAME"
