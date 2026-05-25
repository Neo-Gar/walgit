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
SKIP_BETTERLEAKS="${WALGIT_SKIP_BETTERLEAKS:-0}"

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
fail()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

# Yes/No prompt. $1 = question, $2 = default (y|n, default n). Reads from
# /dev/tty so it still works when this script is piped (`curl … | sh`). With no
# tty available (fully non-interactive), falls back to the default answer.
prompt_yn() {
  _q="$1"; _def="${2:-n}"
  case "$_def" in y|Y) _hint="[Y/n]" ;; *) _hint="[y/N]" ;; esac
  if [ -r /dev/tty ]; then
    printf '%s %s ' "$_q" "$_hint" > /dev/tty
    read -r _ans < /dev/tty || _ans=""
  else
    _ans=""
  fi
  [ -z "$_ans" ] && _ans="$_def"
  case "$_ans" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

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

# ---- betterleaks (secret scanning) -----------------------------------------
# Install methods per upstream (github.com/betterleaks/betterleaks):
#   brew install betterleaks         (macOS + Linuxbrew)
#   sudo dnf install betterleaks     (Fedora/RHEL)
#   docker pull ghcr.io/betterleaks/betterleaks:latest
# Upstream does NOT publish a `go install` path — don't add one.
if [ "$SKIP_BETTERLEAKS" = "1" ]; then
  info "Skipping betterleaks install (WALGIT_SKIP_BETTERLEAKS=1)"
elif command -v betterleaks >/dev/null 2>&1; then
  info "betterleaks already installed: $(betterleaks --version 2>/dev/null | head -n1)"
else
  info "Installing betterleaks (secret scanner)..."
  if command -v brew >/dev/null 2>&1; then
    brew install betterleaks \
      || warn "brew install betterleaks failed; see https://github.com/betterleaks/betterleaks"
  elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y betterleaks \
      || warn "dnf install betterleaks failed; see https://github.com/betterleaks/betterleaks"
  else
    warn "Could not auto-install betterleaks (no brew or dnf found). Install it manually:"
    warn "  macOS / Linuxbrew : brew install betterleaks"
    warn "  Fedora / RHEL     : sudo dnf install betterleaks"
    warn "  Docker            : docker pull ghcr.io/betterleaks/betterleaks:latest"
    warn "  Docs              : https://github.com/betterleaks/betterleaks"
  fi
fi

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

# ---- sui wallet -------------------------------------------------------------
# WalGit needs a Sui address to create repos and pay gas. suiup installs the
# `sui` binary but does not create a wallet, so offer to create one here.
if command -v sui >/dev/null 2>&1; then
  # Only query active-address when a client config already exists — otherwise
  # `sui client` would drop into its interactive first-run setup.
  if [ -f "$HOME/.sui/sui_config/client.yaml" ] && sui client active-address >/dev/null 2>&1; then
    info "Sui wallet detected: $(sui client active-address 2>/dev/null)"
  else
    warn "No Sui wallet/address is configured. WalGit needs one to create repos and pay gas."
    if prompt_yn "Create a Sui wallet now (sui client new-address ed25519 walgit-account)?" "y"; then
      # Redirect stdin from the tty so sui's own prompts (e.g. fullnode setup
      # on first run) reach the user even when this script is piped.
      if [ -r /dev/tty ]; then
        sui client new-address ed25519 walgit-account < /dev/tty \
          || warn "wallet creation failed; create one later: sui client new-address ed25519 walgit-account"
      else
        sui client new-address ed25519 walgit-account \
          || warn "wallet creation failed; create one later: sui client new-address ed25519 walgit-account"
      fi
    else
      info "Skipped. Create a wallet later: sui client new-address ed25519 walgit-account"
    fi
  fi
else
  info "sui not on PATH yet — after reopening your shell, create a wallet:"
  info "  sui client new-address ed25519 walgit-account"
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

Security:
  betterleaks scans for secrets before every push, PR, and MemWal upload.
  $(command -v betterleaks >/dev/null 2>&1 && echo "betterleaks: installed" || echo "betterleaks: not found — scans will be skipped (WALGIT_SKIP_BETTERLEAKS=1 to silence)")

Next steps:
  1. Configure the deployed WalGit package for your network (REQUIRED — every
     on-chain command needs this; without it 'walgit init' will fail):
       walgit config --package-id <PACKAGE_ID> --registry-id <REGISTRY_ID>

  2. Create a repository:
       walgit init <repo>

  3. (Optional) Enable AI reasoning-trace memory — create an account at
     https://memwal.ai (Mainnet) / https://staging.memwal.ai (Testnet), then:
       walgit memwal init

  walgit --help    for all commands

EOF
