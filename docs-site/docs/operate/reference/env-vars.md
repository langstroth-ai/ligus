# Environment variables

Every `LIGUS_*` variable Ligus 0.1.0 reads, from the code. Variables used
only by the test suites are left out.

**Spelling.** Only `LIGUS_*` names are read. A blank value counts as unset. Boolean flags accept `1`/`true`/`on`/`yes` and
`0`/`false`/`off`/`no`, unless noted.

**Desktop or CLI.** The desktop app reads few of these; most configure the
headless `ligus` binary (`ligus serve`, `ligus worker`, ...). The *Read by*
column says which. The relay's own variables are on
[Self-hosting a relay](../networking/self-host.md).

## Connection and identity

| Variable | Default | Read by | Effect |
|---|---|---|---|
| `LIGUS_DATA_DIR` | `~/.ligus` | CLI | the CLI's data dir: identity, keys, `ligus.db`, `config.toml`, serve and tunnel state. The desktop always uses its OS app-data dir (`ai.ligus.desktop`) |
| `LIGUS_RELAY_URL` | from `config.toml` | CLI; desktop on first launch only | relay base URL |
| `LIGUS_RELAY_ACCESS_TOKEN` | from `config.toml` | CLI; desktop on first launch only | relay access token (`hrt1.…` or opaque). A `name:token` form is accepted and the `name:` dropped |
| `LIGUS_WORKSPACE` | `default` | CLI; desktop on first launch only | relay room |
| `LIGUS_WORKSPACE_KEY` | from `config.toml` | CLI; desktop on first launch only | E2EE workspace passphrase. Sync refuses to run without one |
| `LIGUS_RELAY_ISSUER_KEY` | unset | desktop, first launch only | relay issuer seed for minting agent tokens from the People pane (local only, never synced) |
| `LIGUS_IDENTITY_MODE` | ignored | desktop and CLI | ignored: every device verifies and folds by the same rules (a set value logs one warning). The founder's account key is pinned from the invite instead |
| `LIGUS_DISABLE_P2P` | unset | desktop | any value turns off direct peer-to-peer sync (iroh) |
| `LIGUS_NATIVE_PASSKEYS` | on | desktop | `0`: passkey prompts always use the browser hand-off instead of the macOS or Windows passkey dialog ([native passkeys](../security/native-passkeys.md)) |
| `LIGUS_SECRET_BACKEND` | the OS default | desktop, CLI | where secrets live: `keychain`, `secret-service`, `systemd-creds`, `k8s` or `file`. On macOS a keychain that can't be used is an error; `file` is the explicit opt-in to owner-only files ([key storage](../../ligus/concepts/identity.md#key-storage)) |

On the desktop, the connection variables only **seed** `settings.json` the
first time it is created; after that, Settings is the source of truth.

## Agent turns and runtimes

| Variable | Default | Read by | Effect |
|---|---|---|---|
| `LIGUS_WS_SECRET_<ref>` | unset | desktop and CLI | the key for a workspace runtime defined with `secretRef = <ref>`. Set it on each host that runs the agent |
| `LIGUS_WORKER_PERMISSION_MODE` | read-only | CLI worker | `acceptEdits`, `runCommands` or `bypassPermissions` lets worker turns of Claude Code and the subscription CLIs edit files / run commands. Edits land in an isolated worktree as a proposal |
| `LIGUS_WORKER_TOOLS` | on | CLI worker | give workspace HTTP runtimes on this worker the agent tool loop |
| `LIGUS_BUILTIN_TOOLS` | on | CLI | offer the built-in file tools (`read_file`, `list_dir`, `search`, and `write_file`/`edit_file` in an isolated worktree) |
| `LIGUS_BUILTIN_RUN_COMMAND` | off | CLI | also offer `run_command`, only inside the turn's worktree |
| `LIGUS_ALLOW_REMOTE_FILE_TOOLS` | off | CLI | offer file tools even when the model endpoint isn't on this machine, a private network or the tailnet (they send file contents to it) |
| `LIGUS_MCP_AFTER_READS` | off | CLI | **turns off** the exfiltration guard that refuses MCP calls once a turn has read project files |
| `LIGUS_MCP_CONFIG` | unset | CLI | path to a TOML file of MCP servers for headless turns and dashboard panels |
| `LIGUS_MCP_MIN_ROLE` | `admin` for a write-capable turn, `contributor` for a read-only one | desktop, CLI | `viewer`, `contributor`, `admin` or `owner`: the minimum role of a message's author before its turn runs at its configured tier. Below it, Ligus's own tool loop replies tool-free, Claude Code, Antigravity and Copilot run read-only, and Codex, aider, pi and Hermes are refused with a note; an unknown author fails closed. A value below the default logs a warning at startup |
| `LIGUS_SANDBOX` | on | desktop, CLI | `off` runs agent processes without the OS sandbox; write-capable agent turns are then refused (see [Sandbox defaults](../../ligus/concepts/security.md#sandbox-defaults)). Linux: `landlock` skips bubblewrap and uses the [Landlock tier](../ops/linux-sandbox.md); `bwrap` refuses the Landlock fallback |
| `LIGUS_REQUIRE_SANDBOX` | unset | serve, worker, tests | `1`: `ligus serve` and `ligus worker` exit with code 3 instead of starting on a host without an agent sandbox, and the sandbox tests fail instead of printing `SKIPPED`. Any value other than empty, `0`, `false`, `no` or `off` (any case) counts as on, so a typo never turns the guard off |
| `LIGUS_SANDBOX_UNIX_SOCKETS` | unset | desktop and CLI (Linux) | `allow` lets sandboxed agents create unix sockets again (known host sockets stay masked) |
| `LIGUS_SANDBOX_EGRESS_ALLOW` | unset | desktop, CLI | comma-separated `host[:port]` agents may also reach through the egress proxy under the normal envelope (not under dearth mode or a lockdown); `run_command` and pre-flight get only these |
| `LIGUS_SANDBOX_EGRESS_PRESETS` | unset (off) | desktop, CLI | comma-separated [egress presets](../../ligus/concepts/security.md#egress-presets) on for every turn on this host: `package-registries` (npm, crates.io, PyPI, GitHub) and `claude-connectors` (`mcp-proxy.anthropic.com`, Claude Code turns only; lets the agent reach the services you connected to claude.ai). Ignored under dearth mode or a lockdown. On the desktop you can also switch presets per workspace and per chat in **Settings → Guards → Network presets** |
| `LIGUS_SANDBOX_WRITE_ALLOW` | unset | desktop, CLI | comma-separated extra paths agent processes may write (e.g. a shared scratch dir such as `~/scratch/agents`; caches such as `~/.cargo/registry` are the turn's own under its per-turn home) |
| `LIGUS_SANDBOX_READ_ALLOW` | unset | desktop, CLI | paths under `~` an agent process may read after all, credential dirs included (e.g. `~/.aws`); device-local, unlike the signed grants in Settings → Sandbox |
| `LIGUS_SANDBOX_GRANTS` | the default | CLI (`worker`, `serve`, `agent`, `doctor`) | `apply` or `ignore`: whether this host applies the workspace's [signed sandbox grants](../../ligus/concepts/security.md#grants-on-this-device) (paths and agents' shell grants). Unset: only a host whose user is the workspace Owner applies them. The desktop sets this per workspace in Settings → Sandbox |
| `LIGUS_AGENT_TIMEOUT_SECS` | `300` | desktop and CLI | wall-clock budget for one subprocess agent turn (aider, pi, ...) |
| `LIGUS_AGENT_MAX_LINE_BYTES` | `8388608` | desktop and CLI | longest stdout line an agent CLI or ACP adapter may write; a longer one ends the turn |
| `LIGUS_AGENT_MAX_OUTPUT_BYTES` | `268435456` | desktop and CLI | most stdout one agent turn may produce |
| `LIGUS_TURN_IDLE_TIMEOUT_SECS` | `300` | desktop and CLI | how long a streaming model may produce nothing before the turn fails |
| `LIGUS_TURN_TIMEOUT_SECS` | `3600` | desktop and CLI | total backstop for one turn (the desktop adds 300 s) |
| `LIGUS_PATCH_LEAVING_CALLS` | `12` | desktop and CLI | tool calls in a row without measurable progress before the agent must check in; `0` turns the rule off |
| `LIGUS_PREFLIGHT_TEST` | unset | desktop | the test command a proposal-set pre-flight runs. Device-local on purpose: the repository's git config is never read. It runs in the agent sandbox, offline unless `LIGUS_SANDBOX_EGRESS_ALLOW` names hosts or the `package-registries` preset is on |
| `LIGUS_OLLAMA_WIRE` | native | desktop and CLI | `openai` sends Ollama runtimes through the OpenAI-compatible shim (loses `num_ctx`, `keep_alive`, `think`) |
| `LIGUS_ANTHROPIC_MODEL` | `claude-sonnet-4-6` | desktop | fallback model when no default is set |
| `LIGUS_PROVIDER_API_KEY` | unset | desktop and CLI | a provider key that overrides the provider's own variable |
| `LIGUS_RUNTIME_TEST_TIMEOUT_SECS` | `45` | desktop | timeout of the runtime **Test** button |
| `LIGUS_LOG` | `info` | desktop | log filter (`tracing` syntax, for example `debug` or `ligus_runtime=debug`) |
| `LIGUS_CLI_HOME` | per-turn | desktop, CLI | `shared` lets CLI agents write their real config dirs again. By default each turn gets a throwaway home ([details](../../ligus/addons/acp.md#cli-home)) |
| `LIGUS_ACP_INIT_TIMEOUT_SECS` | `60` | desktop, CLI | how long an ACP agent may take to answer `initialize` before the turn falls back to the headless adapter |
| `LIGUS_ACP_ASK_TIMEOUT_SECS` | `600` | desktop, CLI | how long an ACP permission request waits for a person before it's denied |
| `LIGUS_MCP_MAX_TOOL_CALLS` | `64` | desktop, CLI | per-turn budget of [Ligus tool](../../ligus/addons/ligus-mcp.md#budgets) calls |
| `LIGUS_MCP_MAX_PROPOSALS` | `8` | desktop, CLI | proposals per turn |
| `LIGUS_MCP_MAX_HANDOFFS` | `2` | desktop, CLI | hand-offs per turn |
| `LIGUS_MCP_MAX_ASKS` | `3` | desktop, CLI | questions to the chat per turn |
| `LIGUS_MCP_MAX_READ_MESSAGES` | `500` | desktop, CLI | messages and proposals read per turn |
| `LIGUS_MCP_MAX_FORGE_CALLS` | `16` | desktop, CLI | read-only code-host calls (`ligus_forge_*`) per turn |
| `LIGUS_MCP_MAX_CASCADE_TOOL_CALLS` | `192` | desktop, CLI | Ligus tool calls shared by every turn descended from one human message |
| `LIGUS_MCP_MAX_CASCADE_PROPOSALS` | `12` | desktop, CLI | proposals per cascade |
| `LIGUS_MCP_MAX_CASCADE_HANDOFFS` | `6` | desktop, CLI | hand-offs per cascade |
| `LIGUS_MCP_MAX_CASCADE_ASKS` | `6` | desktop, CLI | questions and permission requests per cascade |
| `LIGUS_MCP_MAX_SPEND_USD` | unset | desktop, CLI | spend per turn; reaching it ends the turn ([what is counted](../../ligus/addons/ligus-mcp.md#spend)) |
| `LIGUS_MODEL_PRICES` | unset | desktop, CLI | `prefix=in/out,…` USD per million tokens, for pricing API turns against the spend budget |
| `LIGUS_MCP_BRIDGE` | this binary | desktop, CLI | the program agents run as `ligus mcp-bridge` |

### `ligus agent` (development responder)

| Variable | Default | Effect |
|---|---|---|
| `LIGUS_ALLOW_PERSONAL_AGENT` | unset | must be set for `ligus agent` to run at all |
| `LIGUS_RUNTIME` | unset | workspace runtime id (same as `--runtime`) |
| `LIGUS_PROVIDER` | `anthropic` | `anthropic`, `openai`, `openrouter` or `ollama` when no workspace runtime is used |
| `LIGUS_MODEL` | `claude-sonnet-4-5` | model for that provider |
| `LIGUS_MODEL_BASE_URL` | `http://localhost:11434/v1/chat/completions` | Ollama endpoint |

## `ligus serve`

| Variable | Default | Effect |
|---|---|---|
| `LIGUS_SERVE_LISTEN` | `127.0.0.1:8787` | API bind address. Loopback, private, link-local, CGNAT (Tailscale) or IPv6 ULA only |
| `LIGUS_SERVE_ALLOW_ANY_BIND` | off | allow any address, including `0.0.0.0`. Only behind something else that controls access |
| `LIGUS_SERVE_TOKEN_FILE` | `<data dir>/serve.token` | the direct-path bearer token file (also used by `ligus pair` and `ligus phones revoke`) |
| (no variable) | `<data dir>/serve.operator.token` | the operator's bearer token, created at first start; the only credential for the box owner's routes and secrets on a loopback listener. Never in a pairing link |
| `LIGUS_SERVE_PUBLIC_URL` | `http://<listen>` | URL put in the pairing link |
| `LIGUS_SERVE_LABEL` | `$HOSTNAME`, else `home host` | the host's label |
| `LIGUS_SERVE_NAME` | unset | display name of the box (its own service member) |
| `LIGUS_ADMISSION_FILE` | unset | a file holding a box admission code, redeemed at start-up by `ligus serve` and `ligus worker` (the chart mounts a Secret here); denied to agent processes. See [Admitting a box](../ops/admitting-a-box.md) |
| `LIGUS_SERVE_OWNER` | removed | refused with an error: a box is never a person's device |
| `LIGUS_SERVE_DEFAULT_AGENT` | unset | agent that gets phone messages that mention nobody |
| `LIGUS_SERVE_APPLY_ON_APPROVE` | removed | ignored: there is no auto-apply; a box applies only a person's request naming it ([Apply on a host](../ops/serve-box.md#apply-on-a-host)) |
| `LIGUS_SERVE_PALETTE` / `LIGUS_SERVE_APPEARANCE` | unset | theme for *Match host* |
| `LIGUS_DESKTOP_DATA_DIR` | the OS app-data dir for `ai.ligus.desktop` | where to read the desktop's theme on the same machine |
| `LIGUS_REPOS_DIR` | `<data dir>/repos` | checkouts the phone may bind and clone into |
| `LIGUS_REPOS_MAX_CLONE_MB` | `2048` | clone size cap |
| `LIGUS_GITHUB_APP_CLIENT_ID` | unset | the Ligus GitHub App's client id for device sign-in (also read at build time); no secret |
| `LIGUS_GITLAB_CLIENT_ID` | unset | the gitlab.com OAuth application id for device sign-in (also read at build time); no secret |
| `LIGUS_REPOS_CLONE_TIMEOUT_SECS` | `900` | clone and fetch timeout |
| `LIGUS_REMOTE_RELAY` | from `tunnel/relay.json` | relay for the phone tunnel; `off` disables it for this run |
| `LIGUS_REMOTE_REG_TOKEN` | from `relay.json` | registration token for a gated relay |
| `LIGUS_REMOTE_INSECURE_RELAY` | off | allow an `http://` relay (tests, loopback) |
| `LIGUS_NTFY_URL` / `LIGUS_NTFY_TOPIC` / `LIGUS_NTFY_TOKEN` | unset | notifications through a self-hosted ntfy (URL and topic both, or neither). The URL must be `https://` |
| `LIGUS_NTFY_ALLOW_HTTP` | `0` | `1` accepts a plain `http://` ntfy URL (a server on this host or your tailnet); otherwise serve refuses to start with one |
| `LIGUS_NTFY_TITLES` | `0` | `1` sends who did what and the chat's title to ntfy (never message text); otherwise only generic text such as "A reply is ready" |
| `LIGUS_DASHBOARD_FILE` | `<data dir>/dashboard.toml` | dashboard config |
| `LIGUS_PROMETHEUS_URL` | `http://127.0.0.1:9090` | Prometheus for dashboard panels; when set and no `dashboard.toml` exists, a lab section is generated |
| `LIGUS_LAB_TEMPS` | unset | temperature queries for the generated lab section |

## Install script

| Variable | Default | Effect |
|---|---|---|
| `LIGUS_VERSION` | latest | release `install.sh` installs: `latest`, or a release tag such as `v0.1.0` (or `0.1.0`); anything else is refused |
| `LIGUS_INSTALL_DIR` | `$HOME/.local/bin` | where it installs `ligus` |

`install.sh` downloads `ligus-<target>.sha256` next to the binary from the
same release and refuses to install when the checksum file is missing or
doesn't match.

## Build time

| Variable | Effect |
|---|---|
| `LIGUS_RELEASE_TAG` | baked into release builds for the update check (`dev` otherwise) |
| `LIGUS_GITHUB_APP_CLIENT_ID` / `LIGUS_GITLAB_CLIENT_ID` | also read at build time, as the default client ids for code-host device sign-in (see Runtime above); no secret |
| `LIGUS_APPLE_TEAM_ID` | the Apple team id of a signed macOS release (from the `APPLE_TEAM_ID` release secret): the app trusts the keychain's unlock item only to code that satisfies Ligus's designated requirement for that team |
| `LIGUS_TEST_KEYCHAIN` | `1` runs the macOS keychain tests (`cargo test`), against throwaway keychain files only; never the login keychain. Without it, a test run never copies the real user's CLI sign-ins into a turn home, never bridges Claude Code's keychain item (`cli_home::real_sign_ins_allowed`), and keeps Ligus's own secrets in owner-only files instead of an OS secret store (which on macOS puts an unlock password in the login keychain) |
