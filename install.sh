#!/bin/bash
# pdf2webp installer — downloads the latest release and places it on PATH.
set -e

REPO="manithd/PDF2WebP"
BIN_NAME="pdf2webp"
INSTALL_DIR="/usr/local/bin"

# Detect platform
case "$(uname -s)" in
  Darwin)  OS="apple-darwin"; ASSET="${BIN_NAME}" ;;
  Linux)   OS="unknown-linux-gnu"; ASSET="${BIN_NAME}" ;;
  *)       echo "Unsupported OS: $(uname -s)"; exit 1 ;;
esac

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *)             echo "Unsupported arch: $(uname -m)"; exit 1 ;;
esac

TARGET="${ARCH}-${OS}"

echo "  Downloading ${BIN_NAME} for ${TARGET}..."

# Get the latest release download URL
DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${BIN_NAME}-${TARGET}"

# Download to temp file
TMP_FILE=$(mktemp)
trap 'rm -f "$TMP_FILE"' EXIT

if ! curl -fsSL "$DOWNLOAD_URL" -o "$TMP_FILE"; then
  echo "  Download failed. Trying alternative URL..."
  # Fallback: get the actual binary name from the release
  DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${BIN_NAME}"
  curl -fsSL "$DOWNLOAD_URL" -o "$TMP_FILE" || {
    echo "  Error: could not download ${BIN_NAME}."
    echo "  Please visit https://github.com/${REPO}/releases/latest to download manually."
    exit 1
  }
fi

chmod +x "$TMP_FILE"

echo "  Installing to ${INSTALL_DIR}/${BIN_NAME}..."
if [ ! -w "$INSTALL_DIR" ]; then
  # Need sudo
  sudo mv "$TMP_FILE" "${INSTALL_DIR}/${BIN_NAME}"
else
  mv "$TMP_FILE" "${INSTALL_DIR}/${BIN_NAME}"
fi

echo "  Installed! Run '${BIN_NAME}' to start."
