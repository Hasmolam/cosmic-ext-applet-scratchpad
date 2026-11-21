#!/usr/bin/env bash
# ==============================================================================
# COSMIC Scratchpad Applet (cosmic-ext-applet-scratchpad) - Uninstaller
# Author: Hasan Hüseyin Yolcu <hasanhuseyinyolcu25@gmail.com>
# License: GPL-3.0-only
# ==============================================================================

set -euo pipefail

APP_ID="io.github.hasmolam.cosmic-ext-applet-scratchpad"
BIN_NAME="cosmic-ext-applet-scratchpad"
TARGET_BIN="${HOME}/.local/bin/${BIN_NAME}"
APP_FILE="${HOME}/.local/share/applications/${APP_ID}.desktop"
METAINFO_FILE="${HOME}/.local/share/metainfo/${APP_ID}.metainfo.xml"
ICON_FILE="${HOME}/.local/share/icons/hicolor/scalable/apps/${APP_ID}-symbolic.svg"

RESTART_MODE="prompt"

usage() {
    cat <<'EOF'
Usage: ./uninstall.sh [OPTIONS]

Options:
  -y, --yes         Restart cosmic-panel without prompting
      --no-restart  Skip restarting cosmic-panel
  -h, --help        Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --yes|-y)
            RESTART_MODE="yes"
            shift
            ;;
        --no-restart)
            RESTART_MODE="no"
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

rm -f "${TARGET_BIN}"
rm -f "${APP_FILE}"
rm -f "${METAINFO_FILE}"
rm -f "${ICON_FILE}"

if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
fi

if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

restart_panel() {
    killall "${BIN_NAME}" 2>/dev/null || true
    killall cosmic-panel 2>/dev/null || true
}

case "${RESTART_MODE}" in
    no)
        echo "Run 'killall cosmic-panel' to reload the panel."
        ;;
    yes)
        restart_panel
        ;;
    prompt)
        if [[ -t 0 ]]; then
            read -r -p "Restart cosmic-panel now? [y/N] " reply
            case "${reply}" in
                [yY]|[yY][eE][sS])
                    restart_panel
                    ;;
                *)
                    echo "Run 'killall cosmic-panel' to reload the panel."
                    ;;
            esac
        else
            echo "Run 'killall cosmic-panel' to reload the panel (or pass --yes)."
        fi
        ;;
esac

echo "COSMIC Scratchpad applet uninstalled."
echo "Note: Saved notes in ~/.local/share/cosmic-scratchpad/ were preserved."
