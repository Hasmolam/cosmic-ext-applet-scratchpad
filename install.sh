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

APP_ID="io.github.hasmolam.cosmic-ext-applet-scratchpad"
BIN_NAME="cosmic-ext-applet-scratchpad"
TARGET_DIR="${HOME}/.local/bin"
TARGET_BIN="${TARGET_DIR}/${BIN_NAME}"
APP_DIR="${HOME}/.local/share/applications"
METAINFO_DIR="${HOME}/.local/share/metainfo"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

SCRIPT_PATH="${BASH_SOURCE[0]:-}"
if [[ -n "${SCRIPT_PATH}" ]]; then
    REPO_ROOT="$(cd "$(dirname "${SCRIPT_PATH}")" 2>/dev/null && pwd || pwd)"
else
    REPO_ROOT=""
fi

GITHUB_REPO="${GITHUB_REPO:-Hasmolam/cosmic-ext-applet-scratchpad}"
RELEASE_BASE_URL="${RELEASE_BASE_URL:-https://github.com/${GITHUB_REPO}/releases/latest/download}"
RELEASE_BASE_URL="${RELEASE_BASE_URL%/}"
RAW_BASE_URL="${RAW_BASE_URL:-https://raw.githubusercontent.com/${GITHUB_REPO}/main}"
RAW_BASE_URL="${RAW_BASE_URL%/}"

RESTART_MODE="prompt"

usage() {
    cat <<'EOF'
Usage: ./install.sh [OPTIONS]

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
            echo -e "${RED}[x] Unknown option: $1${NC}" >&2
            usage >&2
            exit 1
            ;;
    esac
done

detect_arch() {
    local machine
    machine="$(uname -m)"
    case "${machine}" in
        x86_64)
            echo "x86_64"
            ;;
        aarch64|arm64)
            echo "aarch64"
            ;;
        *)
            echo -e "${RED}[x] Unsupported architecture: ${machine}. Supported: x86_64, aarch64.${NC}" >&2
            return 1
            ;;
    esac
}

download_file() {
    local url="$1"
    local dest="$2"
    if command -v curl &>/dev/null; then
        curl -fsSL "${url}" -o "${dest}"
    elif command -v wget &>/dev/null; then
        wget -qO "${dest}" "${url}"
    else
        echo -e "${RED}[x] Neither curl nor wget found.${NC}" >&2
        return 1
    fi
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

echo -e "${BLUE}${BOLD}=== COSMIC Scratchpad Applet Installer ===${NC}\n"

mkdir -p "${TARGET_DIR}" "${APP_DIR}" "${METAINFO_DIR}" "${ICON_DIR}"

if [[ ":${PATH}:" != *":${TARGET_DIR}:"* ]]; then
    echo -e "${YELLOW}[!] Warning:${NC} ${TARGET_DIR} is not in your PATH."
    echo -e "    Add 'export PATH=\"\$HOME/.local/bin:\$PATH\"' to your ~/.bashrc or ~/.zshrc."
fi

PACKAGE_BIN="${REPO_ROOT:+${REPO_ROOT}/${BIN_NAME}}"
RELEASE_BIN="${REPO_ROOT:+${REPO_ROOT}/target/release/${BIN_NAME}}"
CARGO_TOML="${REPO_ROOT:+${REPO_ROOT}/Cargo.toml}"

INSTALLED=0

# Option A: Prebuilt binary in package directory
if [[ -n "${PACKAGE_BIN}" && -f "${PACKAGE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found binary in release package.${NC}"
    rm -f "${TARGET_BIN}"
    install -m 0755 "${PACKAGE_BIN}" "${TARGET_BIN}"
    INSTALLED=1

# Option B: Local compiled release binary in repo
elif [[ -n "${RELEASE_BIN}" && -f "${RELEASE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found locally compiled release binary.${NC}"
    rm -f "${TARGET_BIN}"
    install -m 0755 "${RELEASE_BIN}" "${TARGET_BIN}"
    INSTALLED=1

# Option C: Build from source if cargo is available
elif [[ -n "${CARGO_TOML}" && -f "${CARGO_TOML}" ]] && command -v cargo &>/dev/null; then
    echo -e "${BLUE}[*] Building ${BIN_NAME} from source...${NC}"
    (cd "${REPO_ROOT}" && cargo build --release)
    rm -f "${TARGET_BIN}"
    install -m 0755 "${RELEASE_BIN}" "${TARGET_BIN}"
    INSTALLED=1

# Option D: Download prebuilt binary from GitHub Releases and verify SHA-256
else
    ARCH="$(detect_arch)"
    ASSET_NAME="${BIN_NAME}-${ARCH}-linux"
    BIN_URL="${RELEASE_BASE_URL}/${ASSET_NAME}"
    SUMS_URL="${RELEASE_BASE_URL}/SHA256SUMS"

    if ! command -v sha256sum &>/dev/null; then
        echo -e "${RED}[x] sha256sum command is required to verify downloaded binaries.${NC}" >&2
        exit 1
    fi

    echo -e "${BLUE}[*] Downloading ${ASSET_NAME} and SHA256SUMS...${NC}"
    if ! download_file "${BIN_URL}" "${TMP_DIR}/${ASSET_NAME}"; then
        echo -e "${RED}[x] Failed to download ${BIN_URL}.${NC}" >&2
        exit 1
    fi
    if ! download_file "${SUMS_URL}" "${TMP_DIR}/SHA256SUMS"; then
        echo -e "${RED}[x] Failed to download ${SUMS_URL}.${NC}" >&2
        exit 1
    fi

    if ! grep -E "^[0-9a-fA-F]{64}[[:space:]]+\*?${ASSET_NAME}$" "${TMP_DIR}/SHA256SUMS" > "${TMP_DIR}/expected.sha256" || [[ ! -s "${TMP_DIR}/expected.sha256" ]]; then
        echo -e "${RED}[x] Checksum entry for ${ASSET_NAME} not found in SHA256SUMS.${NC}" >&2
        exit 1
    fi

    if ! (cd "${TMP_DIR}" && sha256sum -c expected.sha256); then
        echo -e "${RED}[x] SHA-256 verification failed for ${ASSET_NAME}. Existing installation was not modified.${NC}" >&2
        exit 1
    fi

    rm -f "${TARGET_BIN}"
    install -m 0755 "${TMP_DIR}/${ASSET_NAME}" "${TARGET_BIN}"
    INSTALLED=1
fi

install_data_file() {
    local rel_path="$1"
    local dst_dir="$2"
    local file_name
    file_name="$(basename "${rel_path}")"
    local local_src="${REPO_ROOT:+${REPO_ROOT}/${rel_path}}"

    if [[ -n "${local_src}" && -f "${local_src}" ]]; then
        install -m 0644 "${local_src}" "${dst_dir}/${file_name}"
    else
        echo -e "${BLUE}[*] Downloading ${file_name} from GitHub...${NC}"
        if download_file "${RAW_BASE_URL}/${rel_path}" "${TMP_DIR}/${file_name}"; then
            install -m 0644 "${TMP_DIR}/${file_name}" "${dst_dir}/${file_name}"
        else
            echo -e "${YELLOW}[!] Warning: Failed to download ${file_name}.${NC}" >&2
        fi
    fi
}

install_data_file "data/${APP_ID}.desktop" "${APP_DIR}"
install_data_file "data/${APP_ID}.metainfo.xml" "${METAINFO_DIR}"
install_data_file "data/icons/scalable/apps/${APP_ID}-symbolic.svg" "${ICON_DIR}"

# Refresh desktop and icon database caches
if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

# Handle panel restart and summarize
if [[ "${INSTALLED}" -eq 1 ]]; then
    case "${RESTART_MODE}" in
        no)
            echo "Run 'killall cosmic-panel' to reload the panel."
            ;;
        yes)
            echo -e "\n${BLUE}[*] Restarting cosmic-panel to register applet...${NC}"
            killall cosmic-panel 2>/dev/null || true
            ;;
        prompt)
            if [[ -t 0 ]]; then
                read -r -p "Restart cosmic-panel now? [y/N] " reply
                case "${reply}" in
                    [yY]|[yY][eE][sS])
                        echo -e "\n${BLUE}[*] Restarting cosmic-panel to register applet...${NC}"
                        killall cosmic-panel 2>/dev/null || true
                        ;;
                    *)
                        echo "Run 'killall cosmic-panel' to reload the panel."
                        ;;
                esac
            else
                echo "Run 'killall cosmic-panel' to load the applet (or pass --yes)."
            fi
            ;;
    esac

    echo -e "${GREEN}${BOLD}Success:${NC} COSMIC Scratchpad Applet is installed."
    echo -e "  - Binary installed to: ${TARGET_BIN}"
    echo -e "  - Desktop entry: ${APP_DIR}/${APP_ID}.desktop"
    echo -e "  - AppStream metainfo: ${METAINFO_DIR}/${APP_ID}.metainfo.xml"
    echo -e "  - Available in: COSMIC Settings -> Panel -> Applets (search 'Scratchpad')"
    echo -e "  - Note data stored in: ~/.local/share/cosmic-scratchpad/"
    echo -e "  - To uninstall anytime, run: ./uninstall.sh\n"
fi
