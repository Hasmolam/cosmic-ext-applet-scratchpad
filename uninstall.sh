#!/usr/bin/env bash
# ==============================================================================
# COSMIC Scratchpad Applet (cosmic-ext-applet-scratchpad) - Uninstaller
# Author: Hasan Hüseyin Yolcu <hasanhuseyinyolcu25@gmail.com>
# License: GPL-3.0-only
# ==============================================================================

set -euo pipefail

TARGET_BIN="${HOME}/.local/bin/cosmic-ext-applet-scratchpad"
APP_FILE="${HOME}/.local/share/applications/io.github.hasmolam.cosmic-ext-applet-scratchpad.desktop"
ICON_FILE="${HOME}/.local/share/icons/hicolor/scalable/apps/io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic.svg"

rm -f "${TARGET_BIN}"
rm -f "${APP_FILE}"
rm -f "${ICON_FILE}"

if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
fi

if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

killall cosmic-ext-applet-scratchpad 2>/dev/null || true
killall cosmic-panel 2>/dev/null || true

echo "COSMIC Scratchpad applet successfully uninstalled."
echo "Note: Your saved notes in ~/.local/share/cosmic-scratchpad/ have been preserved."
