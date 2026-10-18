#!/usr/bin/env bash
# N30: remove the user-level nora install. User config is preserved.
set -euo pipefail

PREFIX="${HOME}/.local"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/nora"

echo "==> removing binary"
rm -f "$PREFIX/bin/nora"
rm -f "$PREFIX/libexec/nora/nora"
rmdir "$PREFIX/libexec/nora" 2>/dev/null || true

echo "==> removing terminfo"
rm -f "$HOME/.terminfo/n/nora"
rmdir "$HOME/.terminfo/n" 2>/dev/null || true
rmdir "$HOME/.terminfo" 2>/dev/null || true

echo "==> removing desktop entry"
rm -f "$PREFIX/share/applications/nora.desktop"
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true

echo "==> removing icon"
rm -f "$PREFIX/share/icons/hicolor/128x128/apps/nora.png"
rm -f "$PREFIX/share/icons/hicolor/scalable/apps/nora.svg"

echo "==> preserving user config"
if [ -f "$CONFIG_DIR/config.toml" ]; then
    echo "    config preserved: $CONFIG_DIR/config.toml"
else
    echo "    no user config present"
fi

echo "uninstall complete"
