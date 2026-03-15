#!/bin/sh
set -e

REPO="memoryOSScom/memoryOSS"
INSTALL_DIR="/usr/local/bin"

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  PLATFORM="linux" ;;
  darwin) PLATFORM="darwin" ;;
  *)      echo "Error: unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)             echo "Error: unsupported architecture: $ARCH"; exit 1 ;;
esac

ARTIFACT="memoryoss-${PLATFORM}-${ARCH}"

# Get latest release tag
echo "Fetching latest release..."
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')

if [ -z "$TAG" ]; then
  echo "Error: could not determine latest release"
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${TAG}/${ARTIFACT}.tar.gz"

echo "Downloading memoryOSS ${TAG} (${PLATFORM}/${ARCH})..."
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$URL" -o "$TMPDIR/${ARTIFACT}.tar.gz"
tar xzf "$TMPDIR/${ARTIFACT}.tar.gz" -C "$TMPDIR"

# Install
if [ -w "$INSTALL_DIR" ]; then
  mv "$TMPDIR/memoryoss" "$INSTALL_DIR/memoryoss"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo mv "$TMPDIR/memoryoss" "$INSTALL_DIR/memoryoss"
fi

chmod +x "$INSTALL_DIR/memoryoss"

echo ""
echo "memoryOSS ${TAG} installed to ${INSTALL_DIR}/memoryoss"
echo ""
echo "Get started:"
echo "  memoryoss setup"
echo ""
