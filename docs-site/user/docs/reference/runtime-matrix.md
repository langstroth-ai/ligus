# Runtime matrix

The runtimes Ligus treats as first-class. Every row is run live before each
release with `scripts/release-matrix.sh`, which prints a table for the
release notes. A release ships only when every row passes, or is marked N/A
with a reason.

| Runtime | How Ligus drives it | Ligus tools | Release check |
|---|---|---|---|
| Claude Code | ACP (`claude-agent-acp`, API key only) or `claude -p` | yes (MCP) | live, both drivers |
| Codex | ACP (`codex-acp`) or `codex exec` | over ACP; directives on `codex exec` | live, both drivers |
| GitHub Copilot CLI | ACP (`copilot --acp`) or `copilot -p` | yes (MCP) | live, both drivers |
| Google Antigravity | ACP (`agy acp`) or `agy --print` | over ACP; directives on `agy --print` | live, both drivers |
| Anthropic API | Messages API | yes (folded, tool loop) | live |
| OpenAI API | Chat Completions | yes (folded, with "Supports tools") | live |
| Gemini API | **preset**: OpenAI-compatible endpoint `https://generativelanguage.googleapis.com/v1beta/openai/` (a Custom runtime) | yes (folded) | live |
| llama.cpp | `llama-server` (OpenAI-compatible) | yes (folded) | local job |
| Ollama | native API | yes (folded, when the model supports tools) | local job |
| MLX | **preset**: `mlx_lm.server` on `http://localhost:8080/v1/chat/completions` | yes (folded) | local job (Apple Silicon) |

The Gemini and MLX presets are one-click entries in Settings, Models, both
on the OpenAI-compatible provider. Neither adds a new provider kind.

ACP details, the Claude login rule and the permission mapping:
[Subscription CLIs over ACP](../addons/acp.md). The tools themselves:
[Ligus tools for agents](../addons/ligus-mcp.md).

## Running the matrix {#running}

```sh
scripts/release-matrix.sh                         # every row
ROWS="copilot-acp copilot-legacy" scripts/release-matrix.sh
```

The CLI rows use what is installed and signed in on the machine, and spend a
little subscription quota. A row run on the ACP driver fails instead of
falling back, so the table records which path really ran. For the API rows,
set `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` and `GEMINI_API_KEY`. For the local
rows, start the servers first (`LLAMA_URL`, `OLLAMA_URL`, `MLX_URL`; each
defaults to localhost).
