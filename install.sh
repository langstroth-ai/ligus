#!/bin/sh
# Ligus CLI installer.
#
#   curl -fsSL https://raw.githubusercontent.com/ligus-ai/ligus/main/install.sh | sh
#
# Downloads the prebuilt `ligus` binary for your platform from the latest GitHub
# Release and installs it, plus a `hive` symlink (the former command name, kept
# through v2.1). Override with env vars:
#   LIGUS_VERSION      release tag to install (default: latest)
#   LIGUS_INSTALL_DIR  install directory (default: $HOME/.local/bin)
# The legacy names HIVE_VERSION / HIVE_INSTALL_DIR are still read as fallbacks.
set -eu

REPO="ligus-ai/ligus"
BIN="ligus"

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
  echo "ligus: unsupported platform $os/$arch — build from source: cargo install --path crates/hive-cli" >&2
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

echo "ligus: downloading $BIN ($target, $version)…"
if ! curl -fSL --proto '=https' --tlsv1.2 "$url" -o "$tmp"; then
  echo "ligus: download failed from $url" >&2
  rm -f "$tmp"
  exit 1
fi
chmod +x "$tmp"
mv "$tmp" "$dir/$BIN"
# Former command name: a symlink, unless something else already lives there.
if [ ! -e "$dir/hive" ] || [ -L "$dir/hive" ]; then
  ln -sf "$BIN" "$dir/hive"
fi

echo "ligus: installed to $dir/$BIN"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "ligus: add it to your PATH — export PATH=\"$dir:\$PATH\"" ;;
esac
echo "ligus: run '$BIN --help' to get started."
