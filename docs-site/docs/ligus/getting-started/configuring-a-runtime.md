# Configuring a runtime

A *runtime* is whatever LLM (or CLI agent) actually generates the
replies in a chat. Ligus doesn't talk to a single hard-coded provider
— you configure as many runtimes as you want, and chats pick one per
session.

Open **Settings → Models** to add or edit.

![Settings, Models & runtimes: subscription CLIs, providers and the runtimes this workspace uses (demo data)](../images/desktop-settings-models-light.png#only-light){ width="800" loading=lazy }
![Settings, Models & runtimes: subscription CLIs, providers and the runtimes this workspace uses (demo data)](../images/desktop-settings-models-dark.png#only-dark){ width="800" loading=lazy }

## Supported providers

| Provider          | What it is                                       | Auth                      | Tools? |
|-------------------|--------------------------------------------------|---------------------------|--------|
| `ollama`          | Local Ollama daemon (spoken natively)            | none                      | yes (MCP tools) when the runtime's **Supports tools** switch is on and the model reports tool support |
| `openai`          | OpenAI's API (or any OpenAI-compatible endpoint) | `OPENAI_API_KEY` env / pasted key | yes (MCP tools) |
| `anthropic`       | Anthropic's API                                  | `ANTHROPIC_API_KEY` env / pasted key | yes (MCP tools) |
| `openrouter`      | OpenRouter aggregator                            | `OPENROUTER_API_KEY` env / pasted key | yes (MCP tools) |
| `claude-code`     | Local `claude` CLI (uses your Claude Pro/Max sub) | depends on Claude CLI     | yes (Claude Code's own tools) |
| `aider`           | Local `aider` CLI                                | depends on aider's config | yes (aider's own tools) |
| `pi`              | Local `pi` CLI                                   | depends on pi's config    | yes (pi's own tools) |
| `codex`           | Local `codex` CLI (OpenAI's `codex exec`)        | the CLI's own login       | yes (Codex's own tools) |
| `antigravity`     | Local `agy` CLI (Google Antigravity; uses your Google AI Pro/Ultra sub) | the CLI's own Google sign-in | yes (the CLI's own tools) |
| `copilot`         | Local `copilot` CLI (GitHub Copilot; uses your Copilot sub) | `copilot login` or a token env var | yes (the CLI's own tools) |
| `hermes`          | Any stdin-driven CLI agent (set the executable and flags on the runtime) | depends on the CLI | the CLI's own |
| `ligus-daemon`     | Remote `ligusd` instance                          | bearer token              | passthrough |
| `custom`          | OpenAI-compatible HTTP endpoint                   | varies                    | varies |

`claude-code`, `aider`, `pi`, `codex`, `antigravity`, `copilot` and
`hermes` are the **subprocess** runtimes: Ligus launches a local CLI, which
brings its own tools and its own login, so these runtimes need no API key
in Ligus. To point Ligus at some other OpenAI-compatible server, use
`custom`.

## Provider presets

When you add a runtime in **Settings → Models**, the picker offers
**presets** that pre-fill the endpoint and capability flags so you don't
have to remember them:

- **OpenAI** — `api.openai.com`.
- **OpenRouter** — the OpenRouter aggregator.
- **Azure OpenAI** — an Azure deployment endpoint (your
  `https://<resource>.openai.azure.com` URL + deployment name).
- **Ollama** — your local Ollama daemon.
- **Local gateway (llama.cpp, OpenAI-compatible)** — a `custom` runtime
  for llama.cpp servers behind an OpenAI-compatible gateway, with tools
  on. Replace `<host>` in the endpoint with the gateway's address. See
  [Offline setup with local models](offline-local-models.md).
- **Google Gemini**: the Gemini API's OpenAI-compatible endpoint
  (`https://generativelanguage.googleapis.com/v1beta/openai/`) with your
  Gemini API key.
- **MLX (local, mlx_lm.server)**: Apple Silicon's `mlx_lm.server` on
  `http://localhost:8080`, keyless, with tools on.
- **Custom** — any other **OpenAI-compatible** HTTP endpoint; fill in the
  base URL yourself.

All of these are OpenAI-wire-compatible, so the same multi-turn tool loop works
across them. Pick the closest preset, then adjust the endpoint, model id,
and API key as needed. The [runtime matrix](../reference/runtime-matrix.md)
lists the runtimes checked before every release.

## Runtime config schema

Each runtime is a `[[runtimes]]` block in `ligus.config.toml`. The
Settings UI is a GUI over the same fields:

```toml
[[runtimes]]
id = "anthropic-claude"
name = "Anthropic Claude"
provider = "anthropic"
kind = "remote"
endpoint = "https://api.anthropic.com"
preferred_model = "claude-sonnet-4-5"
supports_tools = true
performance_score = 9.5
cost_per_1m_input_tokens_usd = 3.0
```

For subprocess agents (aider, pi), `endpoint` is the binary path:

```toml
[[runtimes]]
id = "aider"
name = "Aider"
provider = "aider"
kind = "local"
endpoint = "/usr/local/bin/aider"
preferred_model = "gpt-4o"
supports_tools = true
```

See the [Configuration reference](../../operate/reference/config-toml.md) for
every field.

## API providers — environment variables

For API providers, Ligus can read the credential from an environment
variable you control. Set it in your shell config before launching:

```bash
# ~/.zshrc
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
export OPENROUTER_API_KEY="sk-or-..."
```

Environment variables are a convenient fallback — quitting and
relaunching picks up new ones. Note that a key you paste into
**Settings → Models** is **saved to disk** in `settings.json` under
the app data dir (so it persists across launches); it is stored in
plain text, protected only by your OS account and disk encryption. Use
the env-var path if you'd rather the key never be written by Ligus.

## Subprocess agents — installing the binary

Aider and pi are user-supplied CLIs; install them yourself and point
Ligus at the binary:

=== "Aider"

    ```bash
    pip install aider-chat
    which aider
    # → /usr/local/bin/aider
    ```

    Then point Ligus at `/usr/local/bin/aider`.

=== "pi"

    Install the **pi** CLI (an OpenAI-compatible coding-agent backend),
    then point Ligus at its binary:

    ```bash
    which pi
    # → /usr/local/bin/pi
    ```

    Ligus bootstraps a temporary `models.json` and `PI_CODING_AGENT_DIR`
    so `pi` can reach your configured OpenAI-compatible endpoint (see
    `crates/ligus-runtime/src/provider/subprocess.rs`).

For any other OpenAI-compatible server, don't install a binary — add a
**Custom** runtime (`provider = "custom"`) and fill in the base URL, as
described under [Provider presets](#provider-presets).

## Subscription CLIs: Google Antigravity and GitHub Copilot

If you pay for **Google AI Pro / Ultra** or **GitHub Copilot**, Ligus can use
that subscription through the vendor's own CLI, the same way the
`claude-code` runtime uses a Claude subscription. Ligus runs the CLI headless
and streams its reply, including its tool steps. The sign-in stays in the
CLI's own config or your OS keychain: Ligus never reads, copies or syncs it.

| | Google Antigravity | GitHub Copilot |
|---|---|---|
| Binary | `agy` | `copilot` |
| Install | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` | `brew install --cask copilot-cli` or `npm install -g @github/copilot` |
| Sign in (in a terminal on the same machine) | run `agy` once and choose **Sign in with Google** | `copilot login` |

!!! note "Gemini CLI"
    Gemini CLI stopped serving Google AI Pro / Ultra and free personal
    sign-ins on 2026-06-18 (API-key and Code Assist Standard/Enterprise use
    is unaffected). Ligus shows an installed `gemini` binary only to point
    you at `agy`; it can't be added as a runtime.

### Detection in Settings → Models

**Settings → Models → LLM providers** has a **Subscription CLIs** panel that
lists each CLI it looks for:

- **not installed**: the panel shows the install command.
- Installed: the binary's version and its sign-in state:
    - **signed in**: for Copilot, a login in its `config.json`
      (`~/.copilot`, or `$COPILOT_HOME`), or a token in `COPILOT_GITHUB_TOKEN`,
      `GH_TOKEN` or `GITHUB_TOKEN`. Only the token's presence is checked,
      never its value. A classic personal access token (`ghp_…`) reads as
      not signed in, because Copilot CLI doesn't accept one.
    - **not signed in**: the panel shows the command to run in a terminal
      on this machine (`agy` or `copilot login`).
    - **sign-in not checked**: Antigravity keeps its Google sign-in in the OS
      keychain and has no status command, so the panel doesn't know until
      you click **Check Google sign-in**. That runs `agy models`, which is
      read-only and uses no quota, and also fills the model picker with the
      models it lists.
- **Add runtime** adds a keyless runtime for the CLI in one click. Type a
  model first if you want one; blank uses the CLI's default. Once added,
  the row reads **runtime added**.

  For an agent you @mention, you don't need this: **Add an agent** picks
  the CLI, its model and where it runs in one step and sets up the runtime
  itself (see [Add an agent](set-up.md#add-an-agent)).

**Re-scan** checks again after you install or sign in. Detection never signs
you in or out.

Both CLIs follow Ligus's permission setting for Claude Code (**Settings →
Permissions**), mapped to each CLI's own flags; the default is read-only.
The exact flags, and how to set these runtimes up on a `ligus serve` or
`ligus worker` host (`ligus setup runtimes`, or `ligus setup agent` for the guided flow), are in
[Bring-your-own agents](../addons/byo-agents.md#subscription-clis-google-antigravity-and-github-copilot).

In TOML the providers are `antigravity` (or `agy`) and `copilot` (or
`github-copilot`); `endpoint` is the binary, and blank means `agy` or
`copilot` on your `PATH`:

```toml
[[runtimes]]
id = "copilot"
name = "GitHub Copilot CLI"
provider = "copilot"
kind = "local"
endpoint = ""
preferred_model = ""   # blank = the CLI's default model
```

## Picking which runtime a chat uses

When you create a new chat, Ligus picks the workspace's
`default_runtime`. Switch per chat from the route pill under the composer
(`→ @ligus · via …`): it lists every configured runtime, and choosing one
re-points that chat.

<a id="pick-a-model"></a>
## Picking the model for a chat

Each runtime row in the route pill's list has a **›** (or press **→** on
the row). It opens that runtime's models: **Default** (the runtime's own
model) at the top, then the models the harness offers, then **Other…** to
type a name. Picking one switches the chat to that runtime and remembers
the model for this chat. The line under the chat title shows it
(`Runs on MacBook · copilot / gpt-5.3-codex`), and each reply is labelled
with the model it actually ran with. Use the arrow keys to move, Enter to
pick, and Escape or **←** to go back to the runtimes.

The choice is kept on this computer only, per chat and per runtime, and is
never synced:

- It applies to the chat's untagged messages (`@ligus`) that this computer
  answers. In a shared chat those are answered by the computer of the person
  who created the chat; if that isn't you, the list says so, and the other
  person's own model is used. A choice made here never changes another
  member's agent or spends their quota.
- `@`-mentioned agents keep their own runtime and model.
- Switching the chat to another runtime and back brings your choice back.
  **Default** clears it.

Where each list comes from (nothing is sent to a model, so listing spends
no quota; lists are reused for five minutes, and the refresh button asks
again):

| Runtime | List |
|---|---|
| Claude Code | its aliases (`sonnet`, `opus`, `haiku`) and the models Claude Code lists for your account |
| Codex | `codex debug models` |
| GitHub Copilot CLI | the models `copilot help config` names; your plan may not include all of them |
| Google Antigravity (`agy`) | `agy models`, your account's models |
| Ollama | the models installed on the server |
| OpenAI-compatible servers, OpenAI, OpenRouter, Anthropic | the provider's `/models` list |
| pi, aider, Hermes, a named model gateway | none; use **Other…** |

Ligus lists a CLI's models only by running a program named `agy`,
`copilot` or `codex`, in an empty temporary folder, and never sends the
legacy global API key to a custom endpoint. A model id that starts with a
dash is never listed and can't be picked.

**When a model won't run.** If a turn on this computer fails because the
model isn't available (for example a Copilot model your plan doesn't
include), the composer shows one line, such as *claude-opus-4.8 isn't
available to this Copilot account*, with **Pick another model**, instead
of a failed reply. Until you pick that model again, Ligus stops a message
that would use it before sending. For Antigravity, Ollama and local
OpenAI-compatible servers, whose lists are complete, a model that isn't on
the list is caught the same way, and **Other…** won't take it.

Settings → **Models & runtimes** offers the same lists on the runtime
form's model field and on the subscription CLIs' model field, and says when
a typed model isn't on a complete list.

To have a named agent always use a specific runtime, configure a
[Workspace Agent](../concepts/agents.md) in **Settings → Models** and
address it by its `@handle` from the composer.
