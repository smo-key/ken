#!/bin/sh
# Ken installer
#
# Install the latest release of Ken:
#   curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh
#
# Prefer clicking? Download an installer instead:
#   https://github.com/smo-key/ken/releases/latest
#
# What this script does:
#   - figures out your operating system and processor
#   - downloads the matching Ken app and the ken-mcp helper from the
#     latest GitHub release (asset names are the contract with
#     .github/workflows/release.yml: Ken-<target>.app.tar.gz,
#     Ken-<target>.AppImage, ken-mcp-<target>)
#   - if there is no packaged release yet, builds Ken from source
#   - checks whether Claude Code is installed (Ken's AI features use it)
#
# On macOS, Ken installs to /Applications when that's writable, and falls
# back to ~/Applications otherwise - so no administrator account is needed.
#
# Environment overrides (mainly for testing):
#   KEN_INSTALL_PREFIX  install under <prefix>/Applications and <prefix>/bin
#                       instead of /Applications (or ~/Applications) and
#                       ~/.local/bin
#   KEN_DOWNLOAD_BASE   fetch release assets from this base URL instead of
#                       https://github.com/smo-key/ken/releases/latest/download

set -eu

REPO="smo-key/ken"
REPO_URL="https://github.com/$REPO"
DOWNLOAD_BASE="${KEN_DOWNLOAD_BASE:-$REPO_URL/releases/latest/download}"
RELEASES_PAGE="$REPO_URL/releases/latest"

say() { printf '%s\n' "$1"; }
fail() { # fail <line> [more lines...] - print an apology and stop
    printf 'Sorry - %s\n' "$1" >&2
    shift
    while [ "$#" -gt 0 ]; do
        printf '%s\n' "$1" >&2
        shift
    done
    exit 1
}

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/ken-install.XXXXXX")
# shellcheck disable=SC2329 # invoked indirectly via the trap below
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT INT TERM

# --- Figure out where we are running -----------------------------------

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
    Darwin) OS_NAME="macOS" ;;
    Linux) OS_NAME="Linux" ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
        say "You're on Windows - this script is for macOS and Linux."
        say "Please download the Windows installer here instead:"
        say "  $RELEASES_PAGE"
        exit 0
        ;;
    *)
        fail "this script supports macOS and Linux, and this looks like '$OS'." \
            "You can find all downloads at $RELEASES_PAGE"
        ;;
esac

case "$ARCH" in
    arm64 | aarch64) CPU="aarch64" ;;
    x86_64 | amd64) CPU="x86_64" ;;
    *) fail "Ken doesn't have a build for the '$ARCH' processor yet." \
        "You can find all downloads at $RELEASES_PAGE" ;;
esac

if [ "$OS_NAME" = "macOS" ]; then
    TARGET="$CPU-apple-darwin"
    if [ "$CPU" = "aarch64" ]; then CPU_NAME="Apple Silicon"; else CPU_NAME="Intel"; fi
else
    TARGET="$CPU-unknown-linux-gnu"
    CPU_NAME="$CPU"
fi

# Where things get installed. KEN_INSTALL_PREFIX reroutes everything so
# the script can be tested without touching the real system.
if [ -n "${KEN_INSTALL_PREFIX:-}" ]; then
    APP_DIR="$KEN_INSTALL_PREFIX/Applications"
    BIN_DIR="$KEN_INSTALL_PREFIX/bin"
else
    APP_DIR="/Applications"
    BIN_DIR="$HOME/.local/bin"
    # /Applications usually needs an administrator to write to. If it's not
    # writable, install to the user's own Applications folder instead so we
    # never have to ask for an admin password.
    if [ "$OS_NAME" = "macOS" ] && [ ! -w "$APP_DIR" ]; then
        APP_DIR="$HOME/Applications"
        mkdir -p "$APP_DIR"
        say "Note: /Applications needs an administrator, so Ken will go into"
        say "your own Applications folder ($APP_DIR) instead."
    fi
fi

say "Installing Ken for $OS_NAME ($CPU_NAME)."

# --- Small helpers ------------------------------------------------------

command -v curl >/dev/null 2>&1 ||
    fail "this script needs 'curl', which wasn't found." \
        "On Linux, install it with your package manager (e.g. sudo apt install curl)."

# download <url> <destination-file>
# Returns 0 on success, 1 if the file doesn't exist on the server (404).
# Any other problem (no internet, server trouble) stops the script - we
# don't want a flaky connection to silently turn into a source build.
download() {
    dl_url="$1"
    dl_dest="$2"
    dl_code=$(curl -sSL --retry 2 -o "$dl_dest" -w '%{http_code}' "$dl_url") ||
        fail "couldn't connect to download Ken. Please check your internet" \
            "connection and run this again. (URL: $dl_url)"
    case "$dl_code" in
        200 | 000) return 0 ;; # 000 = non-HTTP source (used in tests)
        404)
            rm -f "$dl_dest"
            return 1
            ;;
        *)
            rm -f "$dl_dest"
            fail "the download failed (HTTP $dl_code). Please try again in a" \
                "few minutes, or download Ken by hand from $RELEASES_PAGE"
            ;;
    esac
}

install_binary() { # install_binary <source-file> <name>
    mkdir -p "$BIN_DIR"
    cp "$1" "$BIN_DIR/$2"
    chmod +x "$BIN_DIR/$2"
    say "Installed $2 to $BIN_DIR/$2"
}

install_macos_app() { # install_macos_app <path-to-Ken.app>
    mkdir -p "$APP_DIR"
    if [ ! -w "$APP_DIR" ]; then
        fail "you don't have permission to write to $APP_DIR." \
            "Set KEN_INSTALL_PREFIX to a folder you can write to and run this" \
            "again, or let us know at $REPO_URL/issues"
    fi
    rm -rf "$APP_DIR/Ken.app"
    mv "$1" "$APP_DIR/Ken.app"
    say "Installed Ken to $APP_DIR/Ken.app"
}

path_advice() {
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *)
            say ""
            say "One small thing: $BIN_DIR isn't on your PATH yet, so your"
            say "terminal won't find the commands installed there. To fix it,"
            say "add this line to your shell profile (e.g. ~/.zshrc or ~/.bashrc):"
            say "  export PATH=\"$BIN_DIR:\$PATH\""
            ;;
    esac
}

claude_advice() {
    say ""
    if command -v claude >/dev/null 2>&1; then
        say "Claude Code is installed - Ken's AI features are ready to go."
    else
        say "One more thing: Ken's AI features (ingests, chat, deep research)"
        say "use Claude Code, which isn't installed yet. Everything else -"
        say "indexing, search, editing - works without it. When you're ready:"
        say "  1. Install it:  npm install -g @anthropic-ai/claude-code"
        say "  2. Log in once by running:  claude"
    fi
}

finish() {
    say ""
    if [ "$OS_NAME" = "macOS" ]; then
        say "All done! You'll find Ken in your Applications folder."
    else
        say "All done! Start Ken by running: ken"
    fi
    path_advice
    claude_advice
    exit 0
}

# --- Path 1: install from the latest GitHub release ---------------------

install_from_release() {
    if [ "$OS_NAME" = "macOS" ]; then
        app_asset="Ken-$TARGET.app.tar.gz"
    else
        app_asset="Ken-$TARGET.AppImage"
    fi

    say "Downloading Ken..."
    download "$DOWNLOAD_BASE/$app_asset" "$TMP_DIR/$app_asset" || return 1

    say "Downloading the ken-mcp helper (lets other AI agents use your knowledge base)..."
    download "$DOWNLOAD_BASE/ken-mcp-$TARGET" "$TMP_DIR/ken-mcp" || return 1

    if [ "$OS_NAME" = "macOS" ]; then
        mkdir -p "$TMP_DIR/unpack"
        tar -xzf "$TMP_DIR/$app_asset" -C "$TMP_DIR/unpack"
        [ -d "$TMP_DIR/unpack/Ken.app" ] ||
            fail "the downloaded package didn't contain Ken.app as expected." \
                "Please download Ken by hand from $RELEASES_PAGE"
        install_macos_app "$TMP_DIR/unpack/Ken.app"
    else
        install_binary "$TMP_DIR/$app_asset" "ken"
    fi
    install_binary "$TMP_DIR/ken-mcp" "ken-mcp"
    return 0
}

# --- Path 2: build from source (no packaged release yet) ----------------

check_build_tools() {
    missing=0
    say "First, checking for the tools a build needs..."
    if ! command -v git >/dev/null 2>&1; then
        say "  - git is missing. It usually comes with Xcode tools on macOS"
        say "    (run: xcode-select --install) or your Linux package manager"
        say "    (e.g. sudo apt install git)."
        missing=1
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        say "  - Rust is missing. Install it from https://rustup.rs - it's"
        say "    one command and takes a couple of minutes."
        missing=1
    fi
    if ! command -v node >/dev/null 2>&1; then
        say "  - Node.js is missing. Download it from https://nodejs.org"
        say "    (version 24 or newer)."
        missing=1
    fi
    if ! command -v pnpm >/dev/null 2>&1; then
        say "  - pnpm is missing. Once Node.js is installed, run:"
        say "    npm install -g pnpm"
        missing=1
    fi
    if [ "$missing" -ne 0 ]; then
        say ""
        fail "a few tools are missing (listed above). Install them and run" \
            "this script again - or skip the wait and grab a packaged build" \
            "later from $RELEASES_PAGE"
    fi
    say "All build tools found."
}

install_from_source() {
    say ""
    say "There's no packaged release for your computer yet, so we'll build"
    say "Ken from source. This is automatic but can take 10-20 minutes."
    say ""

    check_build_tools

    # Use the checkout we're already in if this is the Ken repo,
    # otherwise clone a fresh copy.
    if [ -f "src-tauri/tauri.conf.json" ] && [ -f "package.json" ] &&
        grep -q '"name": *"ken"' package.json 2>/dev/null; then
        src_dir=$(pwd)
        say "Building from your current Ken checkout: $src_dir"
    else
        say "Fetching the Ken source code..."
        git clone --depth 1 "$REPO_URL.git" "$TMP_DIR/ken-src"
        src_dir="$TMP_DIR/ken-src"
    fi

    cd "$src_dir"
    say "Installing frontend dependencies..."
    pnpm install
    say "Building the Ken app (this is the long part)..."
    pnpm tauri build

    if [ "$OS_NAME" = "macOS" ]; then
        [ -d "target/release/bundle/macos/Ken.app" ] ||
            fail "the build finished but Ken.app wasn't where we expected" \
                "(target/release/bundle/macos). Please report this at" \
                "$REPO_URL/issues"
        install_macos_app "target/release/bundle/macos/Ken.app"
    else
        appimage=$(find target/release/bundle/appimage -name '*.AppImage' -type f 2>/dev/null | head -n 1)
        [ -n "$appimage" ] ||
            fail "the build finished but no AppImage was produced" \
                "(looked in target/release/bundle/appimage). Please report" \
                "this at $REPO_URL/issues"
        install_binary "$appimage" "ken"
    fi

    say "Building the ken-mcp helper..."
    cargo build --release -p ken-mcp
    install_binary "target/release/ken-mcp" "ken-mcp"
}

# --- Go -----------------------------------------------------------------

if install_from_release; then
    finish
else
    install_from_source
    finish
fi
