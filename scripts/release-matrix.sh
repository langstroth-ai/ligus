#!/usr/bin/env bash
# Ligus release matrix (Phase 0 D.2.4): run every first-class runtime row
# live on this machine and print a table for the release notes. A release
# gate: every row must pass (or be marked N/A with a reason) before a cut.
#
# Uses only what is installed and signed in on this machine. Never run it on
# a box where you don't want subscription quota spent.
#
#   scripts/release-matrix.sh            # every row
#   ROWS="copilot-acp codex-legacy" scripts/release-matrix.sh
#
# API rows need their keys in the env (ANTHROPIC_API_KEY, OPENAI_API_KEY,
# GEMINI_API_KEY); local rows need the server up (LLAMA_URL, OLLAMA_URL,
# MLX_URL default to localhost). Claude Code over ACP needs LIGUS_LIVE_KEY
# (an API key): the claude.ai login is never driven through the SDK adapter.
set -u
cd "$(dirname "$0")/.."

ALL_ROWS="claude-code-acp claude-code-legacy codex-acp codex-legacy copilot-acp copilot-legacy antigravity-acp antigravity-legacy anthropic-api openai-api gemini-api llama-cpp ollama mlx"
ROWS="${ROWS:-$ALL_ROWS}"
# Logs go to a fresh private dir (kept: FAIL rows point into it), or to
# MATRIX_OUT_DIR when set. MATRIX_KEEP_LOGS=0 removes them on exit.
if [ -n "${MATRIX_OUT_DIR:-}" ]; then
  OUT="$MATRIX_OUT_DIR"
  mkdir -p "$OUT"
else
  OUT="$(mktemp -d)"
fi
chmod 700 "$OUT"
# Header files hold keys: never left behind, even on an interrupt.
cleanup() {
  rm -f "$OUT"/.*.hdr
  if [ "${MATRIX_KEEP_LOGS:-1}" = 0 ]; then rm -rf "$OUT"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM
results=()

# Write one header line to a private file for curl's `-H @file`: a key on
# curl's command line is readable by every local process (`ps`) while the
# request runs. printf is a shell builtin, so the key is never in an argv.
header_file() { # name header-line
  local f="$OUT/.$1.hdr"
  (umask 077; printf '%s\n' "$2" >"$f")
  echo "$f"
}

cli_row() { # name driver
  local cli="$1" driver="$2" log="$OUT/$1-$2.log"
  if LIGUS_LIVE_CLI="$cli" LIGUS_LIVE_DRIVER="$driver" cargo test -q -p ligus-runtime --lib live_cli_smoke -- --ignored --nocapture >"$log" 2>&1 \
     && grep -q "MATRIX .* result=ok" "$log"; then
    echo "pass"
  else
    echo "FAIL ($log)"
  fi
}

http_row() { # label endpoint model key
  local label="$1" url="$2" model="$3" key="${4:-}" log="$OUT/$1.log"
  if [ -z "$url" ]; then echo "N/A (no endpoint)"; return; fi
  local auth=()
  [ -n "$key" ] && auth=(-H "@$(header_file "$label" "Authorization: Bearer $key")")
  if curl -sS --max-time 120 ${auth[@]+"${auth[@]}"} -H 'content-type: application/json' "$url" \
      -d "{\"model\":\"$model\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with the word ok and nothing else.\"}]}" >"$log" 2>&1 \
     && grep -qi '"ok' "$log"; then
    echo "pass"
  else
    echo "FAIL ($log)"
  fi
  rm -f "$OUT/.$label.hdr"
}

for row in $ROWS; do
  case "$row" in
    claude-code-acp) r=$( [ -n "${LIGUS_LIVE_KEY:-}" ] && cli_row claude-code acp || echo "N/A (Claude over ACP needs an API key: LIGUS_LIVE_KEY)" ) ;;
    claude-code-legacy) r=$(cli_row claude-code legacy) ;;
    codex-acp) r=$(cli_row codex acp) ;;
    codex-legacy) r=$(cli_row codex legacy) ;;
    copilot-acp) r=$(cli_row copilot acp) ;;
    copilot-legacy) r=$(cli_row copilot legacy) ;;
    antigravity-acp) r=$(cli_row antigravity acp) ;;
    antigravity-legacy) r=$(cli_row antigravity legacy) ;;
    anthropic-api)
      if [ -n "${ANTHROPIC_API_KEY:-}" ] && curl -sS --max-time 120 https://api.anthropic.com/v1/messages \
          -H "@$(header_file anthropic "x-api-key: $ANTHROPIC_API_KEY")" -H 'anthropic-version: 2023-06-01' -H 'content-type: application/json' \
          -d "{\"model\":\"${ANTHROPIC_MODEL:-claude-haiku-4-5}\",\"max_tokens\":16,\"messages\":[{\"role\":\"user\",\"content\":\"Reply with the word ok and nothing else.\"}]}" \
          >"$OUT/anthropic.log" 2>&1 && grep -qi '"ok' "$OUT/anthropic.log"; then r=pass; \
      elif [ -z "${ANTHROPIC_API_KEY:-}" ]; then r="N/A (no ANTHROPIC_API_KEY)"; else r="FAIL ($OUT/anthropic.log)"; fi
      rm -f "$OUT/.anthropic.hdr" ;;
    openai-api) r=$( [ -n "${OPENAI_API_KEY:-}" ] && http_row openai https://api.openai.com/v1/chat/completions "${OPENAI_MODEL:-gpt-5-mini}" "$OPENAI_API_KEY" || echo "N/A (no OPENAI_API_KEY)" ) ;;
    gemini-api) r=$( [ -n "${GEMINI_API_KEY:-}" ] && http_row gemini https://generativelanguage.googleapis.com/v1beta/openai/chat/completions "${GEMINI_MODEL:-gemini-2.5-flash}" "$GEMINI_API_KEY" || echo "N/A (no GEMINI_API_KEY)" ) ;;
    llama-cpp) r=$(http_row llama "${LLAMA_URL:-http://localhost:8080/v1/chat/completions}" "${LLAMA_MODEL:-default}") ;;
    ollama) r=$(LIGUS_OLLAMA_LIVE_ENDPOINT="${OLLAMA_URL:-http://localhost:11434}" cargo test -q -p ligus-runtime --test ollama_live -- --nocapture >"$OUT/ollama.log" 2>&1 && echo pass || echo "FAIL ($OUT/ollama.log)") ;;
    mlx) r=$(http_row mlx "${MLX_URL:-http://localhost:8080/v1/chat/completions}" "${MLX_MODEL:-default}") ;;
    *) r="unknown row" ;;
  esac
  results+=("| $row | $r |")
done

echo
echo "## Release matrix ($(date -u +%Y-%m-%d), $(uname -sm), ligus $(git describe --always --dirty))"
echo
echo "| Runtime | Result |"
echo "|---|---|"
printf '%s\n' "${results[@]}"
echo
if [ "${MATRIX_KEEP_LOGS:-1}" = 0 ]; then echo "Logs: removed (MATRIX_KEEP_LOGS=0)"; else echo "Logs: $OUT"; fi
if printf '%s\n' "${results[@]}" | grep -q FAIL; then exit 1; fi
