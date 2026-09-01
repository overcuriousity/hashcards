#!/usr/bin/env bash
# Installs the latest hashcards release from GitHub.
#
#   curl -fsSL https://raw.githubusercontent.com/overcuriousity/hashcards-web/master/install.sh | sh
#
# Override the install directory with HASHCARDS_INSTALL_DIR (default: ~/.local/bin).
set -eu

REPO="overcuriousity/hashcards-web"
INSTALL_DIR="${HASHCARDS_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
    Linux)
        case "$arch" in
            x86_64) target="linux-amd64" ;;
            *)
                echo "error: unsupported architecture '$arch' on Linux (only x86_64 builds are published)" >&2
                exit 1
                ;;
        esac
        ext="tar.gz"
        ;;
    Darwin)
        case "$arch" in
            arm64) target="macos-arm64" ;;
            *)
                echo "error: unsupported architecture '$arch' on macOS (only arm64 builds are published)" >&2
                exit 1
                ;;
        esac
        ext="tar.gz"
        ;;
    *)
        echo "error: unsupported OS '$os'. On Windows, download the .zip asset from https://github.com/$REPO/releases/latest" >&2
        exit 1
        ;;
esac

echo "Looking up the latest release..."
latest_json="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")"
version="$(printf '%s' "$latest_json" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
if [ -z "$version" ]; then
    echo "error: could not determine the latest release version" >&2
    exit 1
fi

asset="hashcards-${version}-${target}.${ext}"
base_url="https://github.com/$REPO/releases/download/${version}"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo "Downloading ${asset}..."
curl -fsSL -o "$tmp_dir/$asset" "$base_url/$asset"

if curl -fsSL -o "$tmp_dir/$asset.sha256" "$base_url/$asset.sha256" 2>/dev/null; then
    echo "Verifying checksum..."
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$tmp_dir" && sha256sum -c "$asset.sha256")
    else
        (cd "$tmp_dir" && shasum -a 256 -c "$asset.sha256")
    fi
else
    echo "warning: no checksum published for this asset; skipping verification" >&2
fi

echo "Installing to $INSTALL_DIR..."
mkdir -p "$INSTALL_DIR"
tar xzf "$tmp_dir/$asset" -C "$tmp_dir"
install -m 755 "$tmp_dir/hashcards-${version}-${target}/hashcards" "$INSTALL_DIR/hashcards"

echo "Installed hashcards ${version} to $INSTALL_DIR/hashcards"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo "note: $INSTALL_DIR is not on your PATH. Add it with:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac
