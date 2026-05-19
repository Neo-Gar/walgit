#!/usr/bin/env sh
# WalGit installer.
#
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/Neo-Gar/walgit/main/install.sh | sh
#
# Env vars:
#   WALGIT_VERSION   — release tag to install (default: latest)
#   WALGIT_PREFIX    — install prefix for binaries (default: $HOME/.local/bin)
#   WALGIT_SKIP_SUI  — set to 1 to skip suiup/sui install
#   WALGIT_SKIP_WAL  — set to 1 to skip walrus install
#   WALGIT_NETWORK   — sui/walrus network (default: testnet)

set -eu

REPO="Neo-Gar/walgit"
VERSION="${WALGIT_VERSION:-latest}"
PREFIX="${WALGIT_PREFIX:-$HOME/.local/bin}"
NETWORK="${WALGIT_NETWORK:-testnet}"

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
fail()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || fail "required tool not found: $1"; }

need curl
need tar
need uname
need shasum 2>/dev/null || need sha256sum

# ---- detect target ----------------------------------------------------------
detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
        *) fail "unsupported linux arch: $arch" ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        arm64)  echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) fail "unsupported macOS arch: $arch" ;;
      esac
      ;;
    *) fail "unsupported OS: $os" ;;
  esac
}

TARGET="$(detect_target)"
info "Detected target: $TARGET"

# ---- resolve version --------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  info "Resolving latest release..."
  VERSION="$(curl -sSfL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$VERSION" ] || fail "could not determine latest release tag"
fi
VERSION_NO_V="${VERSION#v}"
info "Installing WalGit ${VERSION}"

# ---- check git --------------------------------------------------------------
if command -v git >/dev/null 2>&1; then
  info "git found: $(git --version)"
else
  warn "git is not installed. WalGit wraps git for repo operations and requires it."
  warn "Install git first (e.g. 'brew install git' or 'apt install git'), then re-run this script."
  exit 1
fi

# ---- download and extract WalGit -------------------------------------------
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

ARCHIVE="walgit-${VERSION_NO_V}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

info "Downloading $URL"
curl -sSfL "$URL" -o "$TMPDIR/$ARCHIVE"
curl -sSfL "${URL}.sha256" -o "$TMPDIR/${ARCHIVE}.sha256" || warn "no checksum file published, skipping verification"

if [ -f "$TMPDIR/${ARCHIVE}.sha256" ]; then
  info "Verifying checksum..."
  ( cd "$TMPDIR" && \
    ( command -v shasum >/dev/null 2>&1 && shasum -a 256 -c "${ARCHIVE}.sha256" \
      || sha256sum -c "${ARCHIVE}.sha256" ) ) \
    || fail "checksum mismatch"
fi

info "Extracting..."
tar -xzf "$TMPDIR/$ARCHIVE" -C "$TMPDIR"
EXTRACTED="$TMPDIR/walgit-${VERSION_NO_V}-${TARGET}"

mkdir -p "$PREFIX"
for bin in walgit git-remote-walgit walgit-mcp; do
  install -m 0755 "$EXTRACTED/$bin" "$PREFIX/$bin" 2>/dev/null \
    || { cp "$EXTRACTED/$bin" "$PREFIX/$bin"; chmod 0755 "$PREFIX/$bin"; }
  info "Installed $PREFIX/$bin"
done

# ---- PATH hint --------------------------------------------------------------
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) warn "$PREFIX is not in your PATH. Add this to your shell rc:"
     printf '       export PATH="%s:$PATH"\n' "$PREFIX" ;;
esac

# ---- sui via suiup ----------------------------------------------------------
if [ "${WALGIT_SKIP_SUI:-0}" = "1" ]; then
  info "Skipping sui install (WALGIT_SKIP_SUI=1)"
elif command -v sui >/dev/null 2>&1; then
  info "sui already installed: $(sui --version 2>/dev/null | head -n1)"
else
  info "Installing suiup..."
  curl -sSfL https://raw.githubusercontent.com/Mystenlabs/suiup/main/install.sh | sh
  if command -v suiup >/dev/null 2>&1; then
    info "Installing sui@${NETWORK} via suiup..."
    suiup install "sui@${NETWORK}" || warn "suiup install sui@${NETWORK} failed; run it manually later"
  else
    warn "suiup was installed but is not in PATH. Reopen your shell or add ~/.local/bin to PATH, then run: suiup install sui@${NETWORK}"
  fi
fi

# ---- walrus -----------------------------------------------------------------
if [ "${WALGIT_SKIP_WAL:-0}" = "1" ]; then
  info "Skipping walrus install (WALGIT_SKIP_WAL=1)"
elif command -v walrus >/dev/null 2>&1; then
  info "walrus already installed: $(walrus --version 2>/dev/null | head -n1)"
else
  info "Installing walrus (${NETWORK})..."
  curl -sSf https://install.wal.app | sh -s -- -n "$NETWORK" \
    || warn "walrus installer failed; install it manually from https://install.wal.app"
fi

cat <<EOF

WalGit ${VERSION} installed.

Binaries:
  $PREFIX/walgit
  $PREFIX/git-remote-walgit   (registers walgit:// URLs for git)
  $PREFIX/walgit-mcp          (MCP server)

Next steps:
  walgit --help
  walgit init <repo>

EOF
