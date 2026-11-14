# Settings reference

Where Ligus keeps its settings and every key in each place, checked against
the 2.0.0 code. For what each Settings tab does in the app, see
[Settings](../features/settings.md).

| Store | Scope | Synced? | Edited by |
|---|---|---|---|
| `settings.json` in the desktop's app data dir | this desktop | no | the Settings window |
| `ligus.config.toml` (workspace root, then app data dir) | this device | no | you, and the app when you add runtimes or MCP servers |
| `config.toml` in a CLI data dir | one `ligus` device | no | `ligus enroll` |
| Environment variables | one process | no | you; see [Environment variables](env-vars.md) |
| The workspace config log | the workspace | **yes**, E2EE | Admins and members, through the app and CLI |
| `<repo>/.ligus/runtimes.json`, `mcp-servers.json` | one workspace folder on this device | no | the app |

## Desktop `settings.json`

Created on first launch, seeded from `LIGUS_RELAY_URL` (or
`[transport.relay].endpoint`), `LIGUS_RELAY_ACCESS_TOKEN`, `LIGUS_WORKSPACE`,
`LIGUS_WORKSPACE_KEY`, `LIGUS_RELAY_ISSUER_KEY` and `ANTHROPIC_API_KEY`.
After that the file wins over the environment. If it can't be parsed, the app
runs on the environment seed and doesn't overwrite the file.

**It contains secrets** (workspace keys, tokens, API keys). Keep the data dir
private.

| Key | Default | Meaning |
|---|---|---|
| `relay_url` | none | relay base URL |
| `relay_access_token` | none | bearer token for a gated relay |
| `sync_room` | `default` | relay room of the default workspace |
| `workspace_passphrase` | none | E2EE passphrase (the workspace key) |
| `relay_issuer_key` | none | relay issuer seed for minting agent tokens; local only |
| `api_key` | none | legacy single Anthropic/OpenAI key |
| `claude_permission_mode` | `default` | `default`, `acceptEdits`, `runCommands`, `bypassPermissions` for Claude Code (**Settings → Permissions**) |
| `claude_code_model` | empty | `--model` for Claude Code |
| `summarize_prompt`, `compact_prompt` | none | custom `/summarize` and `/compact` instructions |
| `utility_runtime_id` | none | runtime for quick tasks such as chat titles |
| `builtin_tools` | `true` | offer built-in file tools to HTTP runtimes |
| `builtin_write_in_place` | `false` | allow edits when no isolated worktree is available |
| `builtin_run_command` | `false` | offer `run_command` |
| `retrieval_embed_url`, `retrieval_embed_model`, `retrieval_rerank_url`, `retrieval_rerank_model` | empty (off) | vault retrieval |
| `retrieval_top_k` | `5` (max 20) | passages per turn |
| `retrieval_candidates` | `20` (max 100) | candidates sent to the reranker |
| `flow` | see below | **Settings → Flow** |
| `default_model` | none | falls back to `LIGUS_ANTHROPIC_MODEL`, then `claude-sonnet-4-6` |
| `default_runtime_id` | none | overrides `[app].default_runtime` |
| `local_workspace_id` | none | id of "My workspace" |
| `joined_rooms` | `[]` | |
| `git_email` | none | commit attribution |
| `p2p_secret` | generated | direct peer-to-peer (iroh) secret |
| `ka_secret` | generated | X25519 seed for receiving rotated workspace keys |
| `github_account`, `github_token`, `github_client_id` | none | GitHub identity (`LIGUS_GITHUB_CLIENT_ID` wins) |
| `provider_keys`, `provider_base_urls` | `{}` | per provider: `anthropic`, `openAI`, `openRouter`, `custom` |
| `workspaces` | `[]` | each `{name, relay_url, room, key, icon, dm_account, founder_actor_id}` |
| `dismissed_invites` | `[]` | |
| `agent_templates` | `[]` | `{id, name, runtimeId, role, instructions}` |
| `local_workspace_icon` | none | |
| `schedules` | `[]` | `{id, enabled, label, workspaceId, runtimeId, prompt, trigger, lastRun}`; daily times are UTC |
| `mcp_oauth` | `{}` | per server: OAuth tokens and endpoints |
| `mcp_bearer_tokens`, `mcp_bearer_origins` | `{}` | static MCP bearer tokens, each bound to its server's origin |
| `remote_file_tools_optins` | `{}` | per runtime: allow file tools for this non-private endpoint |
| `workspace_root` | none | legacy last-used folder |
| `workspace_dirs` | `{}` | folder per workspace on this device; never synced |

### `flow`

| Key | Default | Range |
|---|---|---|
| `handoffCards` | `true` | |
| `routeMode` | `shadow` | `off`, `shadow`, `live` |
| `halfLifeDays` | `14` | 0.5 to 365 |
| `cooldownMinutes` | `15` | |
| `epsilonMin` / `epsilonMax` | `0.05` / `0.30` | each at most 0.5 |
| `stopUpstream` | `on` | `on`, `off`, `ab` |
| `queueLimit` | `5` | |
| `longWaitMinutes` | `120` | |
| `recruitApprovers` | `false` | |
| `offerIdleHost` | `false` | |
| `marksInContext` | `true` | |
| `boardAutoPickup` | `false` | |
| `reuseWorktrees` | `true` | |

See [Flow](../features/flow.md).

## `ligus.config.toml`

See [Configuration (TOML)](config-toml.md). Note that the parser ignores
sections it doesn't know (for example `[retrieval]` and `[[runtime_pools]]`
in older examples) without a warning, and that `[[mcp_servers]]` entries
default to `enabled = false` there, while the CLI's `LIGUS_MCP_CONFIG` file
defaults them to `true`.

## CLI `config.toml`

`<LIGUS_DATA_DIR>/config.toml`, written by `ligus enroll`; environment
variables win over it.

| Key | Env override | Meaning |
|---|---|---|
| `relay_url` | `LIGUS_RELAY_URL` | relay base URL |
| `token` | `LIGUS_RELAY_ACCESS_TOKEN` | relay access token |
| `room` | `LIGUS_WORKSPACE` | room (default `default`) |
| `key` | `LIGUS_WORKSPACE_KEY` | **workspace key, in clear** |

## The workspace config log

Workspace-wide state is a stream of signed events that every device checks
on ingest. Its session id is a UUIDv5 of the workspace id. It carries:

| Record | Who may write |
|---|---|
| members and roles | Admin (self-enrolment as Viewer/Contributor through an invite; last Owner protected) |
| device revocations | Admin (the desktop offers revocation only for your own devices) |
| workspace runtimes (they may carry credentials) | Admin |
| agents, hosts, skills, vaults, workflows, channels | Contributor |
| dearth mode, join approvals | Admin |
| chat lockdown | raised by anyone in the workspace (Viewer); cleared by a human Contributor+ |
| leases | Contributor, signed by the holder device |
| Flow path zones, home beacon | Admin |
| Flow route stops, marks, agent profiles | Contributor |
| stale-proposal marks | the hygiene sweep (Contributor) |

## Device-local knobs that are deliberately not synced

- the pre-flight test command (`LIGUS_PREFLIGHT_TEST` or git config
  `ligus.preflightTest`), so a teammate can't make your machine run a
  command;
- workspace folders (`workspace_dirs`) and repo bindings;
- Claude Code permission mode and built-in tool switches;
- MCP OAuth and bearer tokens;
- the relay issuer key.
