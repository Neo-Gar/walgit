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

# Colour support: only when stdout is a terminal and NO_COLOR is unset.
# TRUECOLOR (24-bit) unlocks the gradient banner; otherwise we fall back to a
# single-colour cyan wordmark, then to plain text when there's no tty at all.
USE_COLOR=0
TRUECOLOR=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  USE_COLOR=1
  case "${COLORTERM:-}" in truecolor|24bit) TRUECOLOR=1 ;; esac
fi

# WalGit wordmark — matches the banner printed by the `walgit` CLI (ui.rs),
# painted as a purple→teal gradient that echoes the docs accent colours.
banner() {
  printf '\n'
  set -- \
"   ██╗    ██╗ █████╗ ██╗      ██████╗ ██╗████████╗" \
"   ██║    ██║██╔══██╗██║     ██╔════╝ ██║╚══██╔══╝" \
"   ██║ █╗ ██║███████║██║     ██║  ███╗██║   ██║" \
"   ██║███╗██║██╔══██║██║     ██║   ██║██║   ██║" \
"   ╚███╔███╔╝██║  ██║███████╗╚██████╔╝██║   ██║" \
"    ╚══╝╚══╝ ╚═╝  ╚═╝╚══════╝ ╚═════╝ ╚═╝   ╚═╝"
  if [ "$TRUECOLOR" = "1" ]; then
    _i=1
    for _line in "$@"; do
      case $_i in
        1) _rgb="163;113;247" ;;
        2) _rgb="139;124;248" ;;
        3) _rgb="116;150;240" ;;
        4) _rgb="96;178;232"  ;;
        5) _rgb="86;200;222"  ;;
        *) _rgb="88;217;214"  ;;
      esac
      printf '\033[1;38;2;%sm%s\033[0m\n' "$_rgb" "$_line"
      _i=$((_i + 1))
    done
  elif [ "$USE_COLOR" = "1" ]; then
    for _line in "$@"; do printf '\033[1;36m%s\033[0m\n' "$_line"; done
  else
    for _line in "$@"; do printf '%s\n' "$_line"; done
  fi
  if [ "$USE_COLOR" = "1" ]; then
    printf '   \033[2m%s\033[0m\n\n' "decentralized git on walrus + sui"
  else
    printf '   %s\n\n' "decentralized git on walrus + sui"
  fi
}

banner

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

# ---- PATH setup -------------------------------------------------------------
# Offer to append the PATH export to the user's shell rc so the freshly
# installed binaries are usable in new shells without manual editing.
add_to_path_rc() {
  # Pick the rc file for the user's login shell.
  _shell="$(basename "${SHELL:-}")"
  case "$_shell" in
    zsh)  _rc="$HOME/.zshrc" ;;
    bash) [ -f "$HOME/.bashrc" ] && _rc="$HOME/.bashrc" || _rc="$HOME/.bash_profile" ;;
    fish) _rc="$HOME/.config/fish/config.fish" ;;
    *)    _rc="$HOME/.profile" ;;
  esac
  _line="export PATH=\"$PREFIX:\$PATH\""
  [ "$_shell" = "fish" ] && _line="fish_add_path $PREFIX"
  # Don't double-append if the rc already references PREFIX.
  if [ -f "$_rc" ] && grep -qF "$PREFIX" "$_rc" 2>/dev/null; then
    info "PATH entry for $PREFIX already present in $_rc"
    return 0
  fi
  mkdir -p "$(dirname "$_rc")"
  printf '\n# Added by WalGit installer\n%s\n' "$_line" >> "$_rc" \
    && info "Added $PREFIX to PATH in $_rc — open a new shell or run: $_line" \
    || warn "Could not write to $_rc; add this manually: $_line"
}

case ":$PATH:" in
  *":$PREFIX:"*) ;;  # already on PATH
  *)
    warn "$PREFIX is not in your PATH."
    if prompt_yn "Add $PREFIX to your PATH automatically?" "y"; then
      add_to_path_rc
    else
      warn "Add this to your shell rc yourself:"
      printf '       export PATH="%s:$PATH"\n' "$PREFIX"
    fi
    ;;
esac

# ---- shadowing check --------------------------------------------------------
# A `walgit` earlier in PATH (commonly a prior `cargo install --path cli` in
# ~/.cargo/bin) silently overrides the binary we just installed — the user runs
# an old build without realising. Detect and warn loudly.
_active="$(command -v walgit 2>/dev/null || true)"
if [ -n "$_active" ] && [ "$_active" != "$PREFIX/walgit" ]; then
  warn "Another 'walgit' is ahead of this install in your PATH:"
  warn "    active : $_active"
  warn "    this   : $PREFIX/walgit"
  case "$_active" in
    "$HOME/.cargo/bin/walgit")
      warn "Looks like a previous 'cargo install'. Remove it so the released build wins:"
      warn "    cargo uninstall walgit git-remote-walgit walgit-mcp" ;;
    *)
      warn "Remove it, or put $PREFIX earlier in your PATH, so 'walgit' resolves to this install." ;;
  esac
fi

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

# ---- network / contract setup ----------------------------------------------
# Sponsored mode: the WalGit platform supplies the contract IDs + endpoints, so
# the user never deploys or configures contracts. Opting out means deploying the
# Move contracts in contracts/ yourself and pointing the CLI at them.
WALGIT_BIN="$PREFIX/walgit"
[ -x "$WALGIT_BIN" ] || WALGIT_BIN="walgit"
SPONSORED=0

info "Sponsored mode lets the WalGit platform supply the contract IDs and"
info "Walrus/Seal endpoints automatically — no contract deployment needed."
if prompt_yn "Use sponsored mode?" "y"; then
  if "$WALGIT_BIN" config --sponsored true >/dev/null 2>&1; then
    SPONSORED=1
    info "Sponsored mode enabled."
  else
    warn "Could not enable sponsored mode automatically."
    warn "Run it manually later: walgit config --sponsored true"
  fi
else
  cat <<EOF

Standalone mode — deploy your own WalGit Move contracts (Sui Move):

  1. Fund your Sui address with gas.
     Testnet faucet: https://faucet.sui.io/?network=testnet

  2. Clone the repo to get the contract sources:
       git clone https://github.com/${REPO}.git
       cd walgit/contracts

  3. Publish the contracts (uses your active Sui address for gas):
       sui client publish

  4. From the publish output, copy two IDs:
       - package ID   → "Published Objects" → PackageID
       - registry ID  → "Created Objects" → the shared Registry object
                        (also emitted as the RegistryCreated event)

  5. Point WalGit at them:
       walgit config --package-id <PACKAGE_ID> --registry-id <REGISTRY_ID>

EOF
fi

if [ "$SPONSORED" = "1" ]; then
  SETUP_NOTE="Network is configured via sponsored mode ('walgit config --show' to verify)."
else
  SETUP_NOTE="Deploy + configure your contracts (see the standalone-mode steps above)."
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
  1. ${SETUP_NOTE}

  2. Create a repository:
       walgit init <repo>

  3. (Optional) Enable AI reasoning-trace memory — create an account at
     https://memwal.ai (Mainnet) / https://staging.memwal.ai (Testnet), then:
       walgit memwal init

  walgit --help    for all commands

EOF
