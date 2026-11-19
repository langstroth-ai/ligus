#!/bin/sh
# Ligus CLI installer.
#
#   curl -fsSL https://raw.githubusercontent.com/langstroth-ai/ligus/main/install.sh | sh
#
# Downloads the prebuilt `ligus` binary for your platform from the latest GitHub
# Release, checks it against the release's SHA-256 file (ligus-<target>.sha256)
# and installs it. Override with env vars:
#   LIGUS_VERSION      release tag to install, v0.1.0 or 0.1.0 (default: latest)
#   LIGUS_INSTALL_DIR  install directory (default: $HOME/.local/bin)
set -eu

REPO="langstroth-ai/ligus"
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
  echo "ligus: unsupported platform $os/$arch — build from source: cargo install --path crates/ligus-cli" >&2
  exit 1
fi

version="${LIGUS_VERSION:-latest}"
case "$version" in
  latest) ;;
  v[0-9]*) ;;
  [0-9]*) version="v$version" ;;
  *) echo "ligus: LIGUS_VERSION must be latest or a release like v0.1.0, not '$version'" >&2; exit 1 ;;
esac
case "$version" in
  latest) ;;
  *[!0-9A-Za-z.v-]*)
    echo "ligus: LIGUS_VERSION must be latest or a release like v0.1.0, not '$version'" >&2; exit 1 ;;
esac
if [ "$version" = "latest" ]; then
  url="https://github.com/$REPO/releases/latest/download/$BIN-$target"
else
  url="https://github.com/$REPO/releases/download/$version/$BIN-$target"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  echo "ligus: need sha256sum or shasum to check the download" >&2
  exit 1
fi

dir="${LIGUS_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir"
# Download into the install directory so the final mv is a same-filesystem
# rename, and remove the partial files on any exit.
tmp="$(mktemp "$dir/.$BIN.XXXXXX")"
sum="$(mktemp "$dir/.$BIN.sha256.XXXXXX")"
trap 'rm -f "$tmp" "$sum"' EXIT INT TERM

echo "ligus: downloading $BIN ($target, $version)…"
if ! curl -fSL --proto '=https' --tlsv1.2 "$url" -o "$tmp"; then
  echo "ligus: download failed from $url" >&2
  exit 1
fi
if ! curl -fsSL --proto '=https' --tlsv1.2 "$url.sha256" -o "$sum"; then
  echo "ligus: no checksum file at $url.sha256; not installing" >&2
  exit 1
fi
want="$(awk 'NR == 1 {print $1}' "$sum")"
got="$(sha256 "$tmp")"
if [ -z "$want" ] || [ "$want" != "$got" ]; then
  echo "ligus: checksum mismatch for $BIN-$target (expected ${want:-nothing}, got $got); not installing" >&2
  exit 1
fi
chmod +x "$tmp"
mv "$tmp" "$dir/$BIN"

echo "ligus: installed to $dir/$BIN"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "ligus: add it to your PATH — export PATH=\"$dir:\$PATH\"" ;;
esac
echo "ligus: run '$BIN --help' to get started."
