#!/bin/sh
set -eu

REPO="colin-ho/synapse"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS
case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux)  os="unknown-linux-gnu" ;;
  *)      echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

# Detect architecture
case "$(uname -m)" in
  x86_64|amd64)  arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *)             echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

target="${arch}-${os}"

# Determine version
if [ -n "${SYNAPSE_VERSION:-}" ]; then
  version="$SYNAPSE_VERSION"
else
  version=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)
  if [ -z "$version" ]; then
    echo "Failed to fetch latest version" >&2
    exit 1
  fi
fi

url="https://github.com/${REPO}/releases/download/${version}/synapse-${version}-${target}.tar.gz"
echo "Downloading synapse ${version} for ${target}..."

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

curl -fsSL "$url" | tar xz -C "$tmpdir"

# Install binary
mkdir -p "$INSTALL_DIR"
if [ -w "$INSTALL_DIR" ]; then
  mv "$tmpdir/synapse" "$INSTALL_DIR/synapse"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo mv "$tmpdir/synapse" "$INSTALL_DIR/synapse"
fi

echo "Installed synapse to ${INSTALL_DIR}/synapse"
echo ""
echo "Next: run 'synapse install' to add shell integration to ~/.zshrc"
