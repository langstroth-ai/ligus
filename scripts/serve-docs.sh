#!/usr/bin/env bash
#
# Serve the documentation site (docs-site, https://docs.langstroth.ai/)
# locally with live-reload: Using Ligus under /ligus/, Operating Langstroth
# under /operate/.
#
# First time: installs the pinned docs dependencies into a venv at ./.venv-docs/.
# Subsequent runs: reuses the venv.
#
# Usage:
#   bash scripts/serve-docs.sh [port]
#   open http://127.0.0.1:8000

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

port="${1:-8000}"
case "$port" in
    ''|*[!0-9]*) echo "usage: $0 [port]" >&2; exit 2 ;;
esac

venv_dir="$repo_root/.venv-docs"

if [[ ! -d "$venv_dir" ]]; then
    echo "Setting up docs venv at $venv_dir"
    python3 -m venv "$venv_dir"
    "$venv_dir/bin/pip" install --upgrade pip
fi
"$venv_dir/bin/pip" install --quiet -r docs-site/requirements.txt

exec "$venv_dir/bin/mkdocs" serve -f docs-site/mkdocs.yml --dev-addr "127.0.0.1:$port"
