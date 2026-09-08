#!/bin/sh
# install.sh — install figma-framelink-cli from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/damian-kolasinski/figma-framelink-cli/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version v0.1.0 --dir ~/.local/bin
#
# Env overrides: VERSION (default: latest), INSTALL_DIR, REPO.
# Requires: curl (or wget), tar. No sudo: falls back to ~/.local/bin when
# /usr/local/bin is not writable.
set -eu

REPO="${REPO:-damian-kolasinski/figma-framelink-cli}"
BINARY="figma-framelink-cli"
VERSION="${VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-}"

usage() {
    cat <<EOF
Usage: install.sh [--version VERSION] [--dir DIR]

Installs $BINARY from GitHub Releases (https://github.com/$REPO/releases).

Options:
  --version VERSION   Release tag to install, e.g. v0.1.0 (default: latest).
                      Env: VERSION=...
  --dir DIR           Install directory (default: /usr/local/bin if writable,
                      otherwise ~/.local/bin). Env: INSTALL_DIR=...
  -h, --help          Show this help.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            VERSION="${2:?--version requires a value}"; shift 2 ;;
        --version=*)
            VERSION="${1#--version=}"; shift ;;
        --dir)
            INSTALL_DIR="${2:?--dir requires a value}"; shift 2 ;;
        --dir=*)
            INSTALL_DIR="${1#--dir=}"; shift ;;
        -h|--help)
            usage; exit 0 ;;
        *)
            echo "install.sh: unknown argument: $1" >&2
            usage >&2; exit 1 ;;
    esac
done

# Normalize "0.1.0" -> "v0.1.0"; "latest" stays as-is.
case "$VERSION" in
    latest) ;;
    v*) ;;
    *) VERSION="v$VERSION" ;;
esac

# Detect platform -> Rust target triple matching the release matrix in
# .github/workflows/ci.yml.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *) echo "install.sh: unsupported OS: $os (macOS and Linux only)" >&2; exit 1 ;;
esac
case "$arch" in
    arm64|aarch64) arch_part="aarch64" ;;
    x86_64|amd64) arch_part="x86_64" ;;
    *) echo "install.sh: unsupported architecture: $arch (x86_64 and arm64 only)" >&2; exit 1 ;;
esac
target="$arch_part-$os_part"

if [ "$VERSION" = "latest" ]; then
    url="https://github.com/$REPO/releases/latest/download/$BINARY-$target.tar.gz"
else
    url="https://github.com/$REPO/releases/download/$VERSION/$BINARY-$target.tar.gz"
fi

# Default install dir: /usr/local/bin when writable, else ~/.local/bin
# (piped-from-curl scripts must never surprise with sudo).
if [ -z "$INSTALL_DIR" ]; then
    if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="$HOME/.local/bin"
    fi
fi

download() {
    # $1 = url, $2 = output file
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        echo "install.sh: need curl or wget to download $1" >&2
        exit 1
    fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Installing $BINARY ($VERSION, $target) to $INSTALL_DIR ..."
download "$url" "$tmp/archive.tar.gz"
tar -xzf "$tmp/archive.tar.gz" -C "$tmp"
mkdir -p "$INSTALL_DIR"
cp "$tmp/$BINARY" "$INSTALL_DIR/$BINARY"
chmod +x "$INSTALL_DIR/$BINARY"

if ! "$INSTALL_DIR/$BINARY" --help >/dev/null 2>&1; then
    echo "install.sh: warning: installed binary failed to run --help" >&2
fi

echo "Installed $INSTALL_DIR/$BINARY"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Note: $INSTALL_DIR is not on your PATH. Add it, e.g.:" >&2
       echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" >&2 ;;
esac
