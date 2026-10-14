#!/usr/bin/env bash
# N30: install nora for the current user.
#
# Installs:
#   binary        -> ~/.local/bin/nora
#   terminfo      -> ~/.terminfo (compiled from terminfo/nora.terminfo)
#   desktop entry -> ~/.local/share/applications/nora.desktop
#   icon          -> ~/.local/share/icons/hicolor/128x128/apps/nora.png
#
# User config is preserved: an existing ~/.config/nora/config.toml is
# never touched. When no config exists, the example config is copied in
# as a starting point.
set -euo pipefail

cd "$(dirname "$0")/.."

PREFIX="${HOME}/.local"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/nora"

# Build when the release binary is missing, so `git clone && ./scripts/install.sh`
# works from a clean repository; an existing build is reused.
if [ ! -x target/release/nora ]; then
    echo "==> building nora (release)"
    cargo build --release
fi

echo "==> installing binary to $BIN_DIR"
mkdir -p "$BIN_DIR"
install -m 0755 target/release/nora "$BIN_DIR/nora"
"$BIN_DIR/nora" --version

echo "==> installing terminfo to ~/.terminfo"
if command -v tic >/dev/null 2>&1; then
    # Compile the entry into the per-user terminfo database.
    tic -x -o "$HOME/.terminfo" terminfo/nora.terminfo
else
    # No tic available: fall back to the source file (compiled entries
    # are the documented form; this keeps the install functional).
    mkdir -p "$HOME/.terminfo/n"
    install -m 0644 terminfo/nora.terminfo "$HOME/.terminfo/n/nora"
fi

echo "==> installing desktop entry to $PREFIX/share/applications"
mkdir -p "$PREFIX/share/applications"
install -m 0644 resources/nora.desktop "$PREFIX/share/applications/nora.desktop"
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true

echo "==> installing icon to $PREFIX/share/icons"
ICON_DIR="$PREFIX/share/icons/hicolor/128x128/apps"
mkdir -p "$ICON_DIR"
install -m 0644 resources/nora.png "$ICON_DIR/nora.png"

echo "==> preserving user config"
CONFIG_FILE="$CONFIG_DIR/config.toml"
if [ -f "$CONFIG_FILE" ]; then
    echo "    existing config preserved: $CONFIG_FILE"
else
    mkdir -p "$CONFIG_DIR"
    install -m 0644 resources/nora.example.toml "$CONFIG_FILE"
    echo "    installed example config: $CONFIG_FILE"
fi

echo "install complete: $BIN_DIR/nora"
