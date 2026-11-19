# Subscription CLIs over ACP: verification record

Phase 0 Track D (D.2.3). This is the owner-run record that decides whether
ACP may become the default for Claude Code with a claude.ai subscription.
Until V2 below is confirmed, Claude Code goes over ACP **only with an API key
(or a cloud provider) configured in Ligus**. Subscription users stay on
`claude -p`. The code enforces this in `ligus_runtime::provider::acp::resolve`
(`LegacyReason::ClaudeSubscription`).

Don't name the integration "Claude Code ..." in product copy (SDK naming
rule).

## V1: technical (owner, per CLI)

Run each step on the owner's Mac with Ligus built from the release
candidate. Record the CLI version, the date and the result.

### Claude Code (`claude-agent-acp`)

1. In a clean shell, unset `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
   `CLAUDE_CODE_OAUTH_TOKEN` and every cloud-provider variable. Run only
   `claude /login` (Pro or Max).
2. `npm install -g @agentclientprotocol/claude-agent-acp@0.107.0`.
3. `LIGUS_LIVE_CLI=claude-code LIGUS_LIVE_DRIVER=acp cargo test -p ligus-runtime --lib live_cli_smoke -- --ignored --nocapture`.
   Expected **today**: refused with `ClaudeSubscription` (no key). That is
   the policy gate working.
4. For the technical check only: temporarily allow it in a scratch build
   (comment out the subscription check), then confirm that:
   - `initialize` lists auth methods;
   - `session/prompt` returns text;
   - the turn's usage shows subscription use, not an API key (rate-limit
     events, or the SDK's equivalent).
5. Set `LIGUS_LIVE_KEY=<an API key>` and repeat step 3 on the real build.
   It must pass over ACP and bill the key. Confirm that the key reaches the
   adapter only through the runtime's configured key (the child env
   allow-list), never from the parent shell.

### Codex (`codex-acp`)

1. `codex login` (ChatGPT). Then `npm install -g @agentclientprotocol/codex-acp@2.6.0`.
2. `LIGUS_LIVE_CLI=codex LIGUS_LIVE_DRIVER=acp …live_cli_smoke`. It should
   pass. Record whether `initialize` offers an API-key method and which auth
   method was used.
3. `LIGUS_LIVE_DRIVER=legacy`: it should pass.

### GitHub Copilot CLI (`copilot --acp`)

1. `copilot login`, CLI 1.0.112 or later.
2. `LIGUS_LIVE_CLI=copilot LIGUS_LIVE_DRIVER=acp …live_cli_smoke`, then
   `legacy`. Both should pass.
3. In a real chat at the read-only tier, ask the agent to edit a file. A
   permission question appears in the Review pane; allow it and check that
   the edit lands in the turn's worktree as a proposal.

### Antigravity (`agy acp`)

1. Sign in once (`agy`, or `agy acp --login`). CLI 1.5.0 or later.
2. `LIGUS_LIVE_CLI=antigravity LIGUS_LIVE_DRIVER=acp …live_cli_smoke`, then
   `legacy`.

## V2: policy (Claude only)

Get written confirmation from Anthropic that an open-source client may launch
the user's own Claude Code login through `claude-agent-acp`. Record the
reply (date, who, link) here. Until then, nothing changes in code.

## V3: sandbox (all four)

Repeat V1 for each CLI under the default executor: Seatbelt on macOS, and
bubblewrap on the Linux serve box. Check that:

- the subscription sign-in still works with the per-turn CLI home (C-9).
  On macOS the login keychain stays reachable; the sign-in files are copied
  into the turn home and refreshed tokens copied back;
- the egress proxy allow-list includes the sign-in refresh hosts:
  - `claude.ai`, `console.anthropic.com`;
  - `auth.openai.com`, `chatgpt.com`;
  - `github.com`, `api.github.com`;
  - `oauth2.googleapis.com`, `accounts.google.com`;
- an agent that writes `~/.claude/settings.json` (or `~/.codex/config.toml`,
  and so on) leaves the real file unchanged after the turn.

If a CLI fails only with the per-turn home, set `LIGUS_CLI_HOME=shared` on
that device and record which CLI failed.

## Vendor terms (owner's call)

- Anthropic: Claude Agent SDK overview, the "claude.ai login or rate limits"
  clause: <https://docs.claude.com/en/api/agent-sdk/overview>
- OpenAI: Codex and ChatGPT plan terms: <https://openai.com/policies/>
- GitHub: Copilot product-specific terms: <https://docs.github.com/en/site-policy/github-terms/github-terms-for-additional-products-and-features>
- Google: Antigravity terms: <https://antigravity.google/terms>

## Record

| Date | CLI | Version | Driver | V1 | V3 (macOS) | V3 (Linux) | Notes |
|---|---|---|---|---|---|---|---|
| | | | | | | | |
