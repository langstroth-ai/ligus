# Onboarding

Ligus's first run appears automatically the first time you open the app on
a fresh data dir. It is gated on the data dir itself: it shows until
`settings.json` in `ai.ligus.desktop/` records `"onboarded": true`, which
finishing it writes. Nothing in the webview's own storage counts, and
neither does a display name, so deleting the data dir (or resetting it)
always brings the first run back with a new identity. The feature tour that
runs after it is tracked the same way (`"tour_seen"`).

## The first question

Before it asks for a name, the first run asks **Is this your first device
with Ligus?**

- **Yes, start here** goes through three steps (the dots at the top count
  them off).
- **I already use Ligus on another device** links this device to your
  account instead. It asks for no name and makes or joins no workspace, so
  you never end up as a second person in your own workspaces: you paste the
  approval address from your other device, both screens show the same code,
  and your other device confirms with your passkey (see
  [From a second desktop](linked-devices.md#desktop)). The workspace and
  your profile then come from your other device. The first run ends there,
  unless your account has no passkey yet: then one screen says to add it on
  your first device. **Back** returns to the question.

The answer is kept in `settings.json` (`"first_run_path"`), where the
invite guard below reads it.

| Step   | Captures                                             |
|--------|------------------------------------------------------|
| You    | Display name (prefilled from git) and an optional picture; your safety code is behind the **?** |
| Secure | Makes your first workspace (named after you, for example "Ana's workspace"), then a passkey and your recovery key, from one button |
| Agent  | One agent (Claude Code / Codex / OpenAI-compatible / Anthropic API key / Ollama) + "Let agents edit files and run commands" |

**Secure can't be left half-done.** There is no Back once the workspace
exists. The only way past it before it's finished is **Finish later**, which
asks first ("Finish later?") and says what stays locked until your passkey
and recovery key exist: inviting people and other changes that need your
passkey. Finish it any time in **Settings → Passkeys & keys**. If the app
quits during Secure, the next first run picks up the same workspace instead
of making another (`"first_run_workspace"` in `settings.json`).

The project folder, the relay and your team aren't asked here. They come up
when they first mean something: when you pick a folder, invite someone or
join a team (**＋** in the workspace rail). The theme is in **Settings →
Appearance**.

The runtime picker offers Claude Code, Codex, OpenAI-compatible
(OpenAI/OpenRouter/local), an Anthropic API key, and Ollama (shown only when
a local Ollama daemon is detected); aider and pi are configured later from
Settings, not here. The **"Let agents edit files and run commands"**
checkbox sets Claude Code's permission mode. For an API key, **Finish**
first checks that the key answers; a second **Finish** goes on anyway.

## The invite guard

When you paste an invite or a short code under **＋ → Join**, and it looks
like you already use Ligus on another device, one line says **Looks like you
already use Ligus. Link this device instead.** It's a hint only: **Join
workspace** still works, and invites stay for other people. It shows when
this device's first run was answered "I already use Ligus", when this device
is already linked to an account, or when the invite is for a workspace your
own account made that isn't on this device. An invite for a workspace this
device already has shows nothing.

The first run persists your settings and identity into the app data dir
(`ai.ligus.desktop/`): public account/device records as JSON, private
key seeds under `keys/`, and `settings.json` (including any API key you
paste). (Key seeds are files, not in the OS keystore — see
[Identity & devices](../concepts/identity.md).)

## Re-running onboarding

The first run returns whenever the data dir is new. The supported way to
start over is **Settings → Danger zone → Reset local data…**,
which wipes this device's chats, identity, keys, settings, and
workspaces and relaunches Ligus fresh into the first run. Quitting Ligus and
deleting `ai.ligus.desktop/` has the same effect.

## Headless / pre-seeded config

You can pre-populate runtimes and transport by shipping a
`ligus.config.toml` in your workspace root; the Settings UI is a GUI
over the same fields. Note this does **not** skip the first run on its
own — onboarding still runs until it has been finished on this data dir.
Minimum viable config:

```toml
[app]
name = "Ligus"
local_mode = true
sync_mode = "local"
default_runtime = "anthropic"
default_model = "claude-sonnet-4-5"

[transport]
kind = "local"

[sync]
enabled = false
server = ""
device_name = "my-mac"
end_to_end_encryption = true

[retrieval]
mode = "manual"
top_k = 8
chunk_size = 1200
keyword_weight = 0.35
semantic_weight = 0.65
recency_bias = false

[[runtimes]]
id = "anthropic"
name = "Anthropic"
provider = "anthropic"
kind = "remote"
endpoint = "https://api.anthropic.com"
preferred_model = "claude-sonnet-4-5"
supports_tools = true

[chat_defaults]
permission_preset = "default"
retrieval_mode = "manual"
show_context_panel = true
show_activity_panel = true
```

See the [Configuration reference](../../operate/reference/config-toml.md) for
the full schema.
