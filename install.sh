#!/usr/bin/env bash
# ==============================================================================
# COSMIC Scratchpad Applet (cosmic-ext-applet-scratchpad) - Installer
# Author: Hasan Hüseyin Yolcu <hasanhuseyinyolcu25@gmail.com>
# License: GPL-3.0-only
# ==============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
NC='\033[0m'

TARGET_DIR="${HOME}/.local/bin"
TARGET_BIN="${TARGET_DIR}/cosmic-ext-applet-scratchpad"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd || pwd)"
PACKAGE_BIN="${REPO_ROOT}/cosmic-ext-applet-scratchpad"
RELEASE_BIN="${REPO_ROOT}/target/release/cosmic-ext-applet-scratchpad"
GITHUB_REPO="Hasmolam/cosmic-ext-applet-scratchpad"
RAW_BASE_URL="https://raw.githubusercontent.com/${GITHUB_REPO}/main"

for arg in "$@"; do
    case "$arg" in
        --help|-h)
            echo "Usage: ./install.sh [OPTIONS]"
            echo "Options:"
            echo "  --help, -h    Show this help message"
            exit 0
            ;;
    esac
done

echo -e "${BLUE}${BOLD}=== COSMIC Scratchpad Applet Installer ===${NC}\n"

mkdir -p "${TARGET_DIR}" "${APP_DIR}" "${ICON_DIR}"

if [[ ":${PATH}:" != *":${TARGET_DIR}:"* ]]; then
    echo -e "${YELLOW}[!] Warning:${NC} ${TARGET_DIR} is not in your PATH."
    echo -e "    Add 'export PATH=\"\$HOME/.local/bin:\$PATH\"' to your ~/.bashrc or ~/.zshrc."
fi

INSTALLED=0

# Clean old destination binary to prevent ETXTBSY if running
rm -f "${TARGET_BIN}"

# Option A: Prebuilt binary in package directory
if [[ -f "${PACKAGE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found binary in release package.${NC}"
    install -m 0755 "${PACKAGE_BIN}" "${TARGET_BIN}"
    INSTALLED=1

# Option B: Local compiled release binary in repo
elif [[ -f "${RELEASE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found locally compiled release binary.${NC}"
    install -m 0755 "${RELEASE_BIN}" "${TARGET_BIN}"
    INSTALLED=1

# Option C: Build from source if cargo is available
elif command -v cargo &>/dev/null && [[ -f "${REPO_ROOT}/Cargo.toml" ]]; then
    echo -e "${BLUE}[*] Building cosmic-ext-applet-scratchpad from source...${NC}"
    (cd "${REPO_ROOT}" && cargo build --release)
    install -m 0755 "${RELEASE_BIN}" "${TARGET_BIN}"
    INSTALLED=1

# Option D: Download latest prebuilt binary from GitHub Releases
else
    echo -e "${BLUE}[*] Downloading latest prebuilt binary from GitHub Releases...${NC}"
    DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/latest/download/cosmic-ext-applet-scratchpad"
    TMP_BIN=$(mktemp)
    if command -v curl &>/dev/null; then
        curl -fsSL "${DOWNLOAD_URL}" -o "${TMP_BIN}" || {
            echo -e "${RED}[x] Failed to download prebuilt binary.${NC}"
            rm -f "${TMP_BIN}"
            exit 1
        }
    elif command -v wget &>/dev/null; then
        wget -qO "${TMP_BIN}" "${DOWNLOAD_URL}" || {
            echo -e "${RED}[x] Failed to download prebuilt binary.${NC}"
            rm -f "${TMP_BIN}"
            exit 1
        }
    else
        echo -e "${RED}[x] Neither curl nor wget found.${NC}"
        rm -f "${TMP_BIN}"
        exit 1
    fi
    install -m 0755 "${TMP_BIN}" "${TARGET_BIN}"
    rm -f "${TMP_BIN}"
    INSTALLED=1
fi

# Install desktop entry and symbolic icon
DESKTOP_SRC="${REPO_ROOT}/data/io.github.hasmolam.cosmic-ext-applet-scratchpad.desktop"
ICON_SRC="${REPO_ROOT}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic.svg"

if [[ -f "${DESKTOP_SRC}" ]]; then
    cp "${DESKTOP_SRC}" "${APP_DIR}/"
else
    echo -e "${BLUE}[*] Downloading desktop file from GitHub...${NC}"
    if command -v curl &>/dev/null; then
        curl -fsSL "${RAW_BASE_URL}/data/io.github.hasmolam.cosmic-ext-applet-scratchpad.desktop" -o "${APP_DIR}/io.github.hasmolam.cosmic-ext-applet-scratchpad.desktop" || true
    elif command -v wget &>/dev/null; then
        wget -qO "${APP_DIR}/io.github.hasmolam.cosmic-ext-applet-scratchpad.desktop" "${RAW_BASE_URL}/data/io.github.hasmolam.cosmic-ext-applet-scratchpad.desktop" || true
    fi
fi

if [[ -f "${ICON_SRC}" ]]; then
    cp "${ICON_SRC}" "${ICON_DIR}/"
else
    echo -e "${BLUE}[*] Downloading applet icon from GitHub...${NC}"
    if command -v curl &>/dev/null; then
        curl -fsSL "${RAW_BASE_URL}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic.svg" -o "${ICON_DIR}/io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic.svg" || true
    elif command -v wget &>/dev/null; then
        wget -qO "${ICON_DIR}/io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic.svg" "${RAW_BASE_URL}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic.svg" || true
    fi
fi

# Refresh desktop and icon database caches
if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

# Restart panel and summarize
if [[ "${INSTALLED}" -eq 1 ]]; then
    echo -e "\n${BLUE}[*] Restarting cosmic-panel to register applet...${NC}"
    killall cosmic-panel 2>/dev/null || true
    echo -e "${GREEN}${BOLD}✓ Success!${NC} COSMIC Scratchpad Applet is installed."
    echo -e "  - Binary installed to: ${TARGET_BIN}"
    echo -e "  - Available in: COSMIC Settings -> Panel -> Applets (search 'Scratchpad')"
    echo -e "  - Note data stored in: ~/.local/share/cosmic-scratchpad/"
    echo -e "  - To uninstall anytime, run: ./uninstall.sh\n"
fi
