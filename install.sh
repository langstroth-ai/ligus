#!/bin/sh
# Hive CLI installer.
#
#   curl -fsSL https://raw.githubusercontent.com/ligus-ai/hive/main/install.sh | sh
#
# Downloads the prebuilt `hive` binary for your platform from the latest GitHub
# Release and installs it. Override with env vars:
#   LIGUS_VERSION      release tag to install (default: latest)
#   LIGUS_INSTALL_DIR  install directory (default: $HOME/.local/bin)
# The legacy names HIVE_VERSION / HIVE_INSTALL_DIR are still read as fallbacks.
set -eu

REPO="ligus-ai/hive"
BIN="hive"

os="$(uname -s)"
arch="$(uname -m)"
target=""
case "$os" in
  Darwin)
    case "$arch" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-musl" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
    esac ;;
esac

if [ -z "$target" ]; then
  echo "hive: unsupported platform $os/$arch — build from source: cargo install --path crates/hive-cli" >&2
  exit 1
fi

version="${LIGUS_VERSION:-${HIVE_VERSION:-latest}}"
if [ "$version" = "latest" ]; then
  url="https://github.com/$REPO/releases/latest/download/$BIN-$target"
else
  url="https://github.com/$REPO/releases/download/$version/$BIN-$target"
fi

dir="${LIGUS_INSTALL_DIR:-${HIVE_INSTALL_DIR:-$HOME/.local/bin}}"
mkdir -p "$dir"
tmp="$(mktemp)"

echo "hive: downloading $BIN ($target, $version)…"
if ! curl -fSL --proto '=https' --tlsv1.2 "$url" -o "$tmp"; then
  echo "hive: download failed from $url" >&2
  rm -f "$tmp"
  exit 1
fi
chmod +x "$tmp"
mv "$tmp" "$dir/$BIN"

echo "hive: installed to $dir/$BIN"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "hive: add it to your PATH — export PATH=\"$dir:\$PATH\"" ;;
esac
echo "hive: run '$BIN --help' to get started."
