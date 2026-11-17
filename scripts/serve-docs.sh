#!/usr/bin/env bash
#
# Serve one of the two documentation sites locally with live-reload.
#
#   user      docs-site/user      -> docs.ligus.ai       (default)
#   operator  docs-site/operator  -> docs.langstroth.ai
#
# First time: installs the pinned docs dependencies into a venv at ./.venv-docs/.
# Subsequent runs: reuses the venv.
#
# Usage:
#   bash scripts/serve-docs.sh [user|operator] [port]
#   open http://127.0.0.1:8000

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

site="${1:-user}"
port="${2:-8000}"
case "$site" in
    user|operator) ;;
    *) echo "usage: $0 [user|operator] [port]" >&2; exit 2 ;;
esac

venv_dir="$repo_root/.venv-docs"

if [[ ! -d "$venv_dir" ]]; then
    echo "Setting up docs venv at $venv_dir"
    python3 -m venv "$venv_dir"
    "$venv_dir/bin/pip" install --upgrade pip
fi
"$venv_dir/bin/pip" install --quiet -r docs-site/requirements.txt

exec "$venv_dir/bin/mkdocs" serve -f "docs-site/$site/mkdocs.yml" --dev-addr "127.0.0.1:$port"
