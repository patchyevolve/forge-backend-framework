#!/bin/sh
# Forge — one-liner installer
# Run it with: curl -fsSL https://github.com/anomalyco/forge/releases/latest/download/install.sh | sh
#
# What it does:
#   1. Figures out your OS and CPU type
#   2. Downloads the right prebuilt binary for your platform
#   3. Verifies the SHA-256 checksum so nothing's busted
#   4. Installs it to ~/.local/bin/forge (or /usr/local/bin/forge if you're root)

set -eu

REPO="anomalyco/forge"
VERSION="${FORGE_VERSION:-latest}"
INSTALL_DIR="${FORGE_INSTALL_DIR:-}"

# --- What OS and CPU are we running on? ---
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  OS="unknown-linux-musl" ;;
    Darwin) OS="apple-darwin" ;;
    *)      echo "unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)            echo "unsupported arch: $ARCH"; exit 1 ;;
esac

# x86_64-unknown-linux-musl is the main build — what CI tests
# aarch64-unknown-linux-musl covers ARM hardware (Pi, etc.)
# macOS targets exist but haven't run through CI yet
TARGET="${ARCH}-${OS}"

# --- Decide where the binary goes ---
if [ -z "$INSTALL_DIR" ]; then
    if [ "$(id -u)" -eq 0 ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="${HOME}/.local/bin"
    fi
fi

# --- Pull the archive and the checksum file ---
if [ "$VERSION" = "latest" ]; then
    BASE_URL="https://github.com/${REPO}/releases/latest/download"
else
    BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

ARCHIVE="forge-${TARGET}.tar.gz"
CHECKSUM="${ARCHIVE}.sha256"

echo "Downloading Forge for ${TARGET}..."
curl -fsSL "${BASE_URL}/${ARCHIVE}" -o "/tmp/${ARCHIVE}"
curl -fsSL "${BASE_URL}/${CHECKSUM}" -o "/tmp/${CHECKSUM}"

# --- Check the hash so we know the download's not corrupted ---
echo "Verifying checksum..."
# Linux ships sha256sum; macOS has shasum -a 256
if command -v sha256sum >/dev/null 2>&1; then
    (cd /tmp && sha256sum -c "${CHECKSUM}")
elif command -v shasum >/dev/null 2>&1; then
    (cd /tmp && shasum -a 256 -c "${CHECKSUM}")
else
    echo "WARNING: no SHA-256 utility found — skipping checksum verification"
fi

# --- Extract the binary and put it on the PATH ---
mkdir -p "$INSTALL_DIR"
tar xzf "/tmp/${ARCHIVE}" -C "$INSTALL_DIR" forge
chmod +x "${INSTALL_DIR}/forge"
rm -f "/tmp/${ARCHIVE}" "/tmp/${CHECKSUM}"

echo "Forge ${VERSION} installed to ${INSTALL_DIR}/forge"
echo "Run 'forge --help' to get started."
