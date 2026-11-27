# First launch

Ligus is a desktop app for macOS, Windows and Linux. On first launch it
asks whether this is your first device with Ligus, then sets up your name,
secures your first workspace and adds an agent.

## Install

On macOS, install with Homebrew:

```bash
brew install --cask langstroth-ai/ligus/ligus-app
```

Or grab a bundle for your OS from the
[releases page](https://github.com/langstroth-ai/ligus/releases), or build from source:

```bash
git clone https://github.com/langstroth-ai/ligus.git
cd ligus
cargo tauri build        # bundles for the current OS → target/release/bundle/
# or a fast dev loop without bundling:
cargo tauri dev
```

See [Building the dist](../../operate/ops/build.md) for per-OS details and cross-builds.
Release DMGs for macOS (and the Homebrew cask) are signed and notarized, so
they open normally. Windows and Linux release builds, and anything you build
from source, are unsigned, so first launch needs a nudge:

- **macOS:** right-click → **Open** (or `xattr -dr com.apple.quarantine /Applications/Ligus.app`).
- **Windows:** SmartScreen → **More info → Run anyway**.
- **Linux:** `chmod +x Ligus_*.AppImage && ./Ligus_*.AppImage`.

## First run

The first run opens on one question: **Is this your first device with
Ligus?**

- **I already use Ligus on another device**: this computer becomes one
  more of your devices, not a new you (starting again would make a second
  you, with its own name and keys). It asks no name and makes no workspace.
  On your other device, open **Settings → Profile → My devices → Add a
  computer** and copy the approval address; paste it here and choose
  **Continue**. Both screens show the same code: check it, choose **Codes
  match**, and confirm on your other device with your passkey. Then choose
  **Link** when this computer asks **Link this computer as Ana?**. Your
  workspace and your profile come from your other device. If your relay
  can't carry the request, choose **Use a request code instead** (see
  [From a second desktop](../features/linked-devices.md#desktop)). **Back**
  returns to the question.
- **Yes, start here**: three steps, counted off by the dots at the top.

1. **You**: a display name (prefilled from git) and, optionally, a
   picture. Ligus has already made your account key and this device's key
   behind the scenes; your safety code is behind the **?** (see below).
2. **Secure**: Ligus makes your first workspace, named after you, then one
   button creates a passkey and your recovery key (24 words, shown once:
   write them down). The passkey confirms changes to who's in a workspace
   and what agents may do. The only way past this step before it's done is
   **Finish later**, which asks first; finish it any time in **Settings →
   Passkeys & keys**.
3. **Agent**: the **Add an agent** sheet: a name, what answers (the
   coding CLIs on this Mac with their sign-in state, a model server such as
   Ollama, an Anthropic or OpenAI-compatible API key, and anything your
   hosts offer), its model and where it runs. Ligus picks the best one it
   finds, with a test turn on. A **"Let agents edit files and run
   commands"** checkbox sets the permission mode. One button adds the agent
   (see [Add an agent](set-up.md#add-an-agent)).

![Onboarding, the agent step: Claude Code detected, other harnesses and API keys listed (demo data)](../images/desktop-onboarding-agent-light.png#only-light){ width="560" loading=lazy }
![Onboarding, the agent step: Claude Code detected, other harnesses and API keys listed (demo data)](../images/desktop-onboarding-agent-dark.png#only-dark){ width="560" loading=lazy }

The project folder, the relay and your team come up when they first mean
something: pick a folder from the sidebar, and invite people or join a
team from **＋** in the workspace rail (the
[community relay](../../operate/networking/community-relay.md) is
preselected there). The theme is in **Settings → Appearance**. See
[Onboarding](../features/onboarding.md) for the details.

## What gets written

After the first run:

- The app data dir `ai.ligus.desktop/` — the event store `ligus.db`
  (every workspace's signed events), your account + device records
  (public bits) in `identity.json`, private key seeds under `keys/`, and
  `settings.json` (runtime/provider config, including any API keys and the
  workspace keys you paste into Settings).
- `<workspace>/ligus.config.toml` — optional file-based runtime +
  transport config.
- `<workspace>/.ligus/` — per-chat git worktrees (`worktrees/`), saved
  patches of interrupted turns (`stopped-turns/`) and the workspace's
  `runtimes.json` / `mcp-servers.json`.

Everything is local. Nothing leaves your machine unless you
explicitly configure a transport (relay endpoint or LAN discovery
toggle).

## Your account and safety code

There is no sign-in. On first launch Ligus makes an account key and a random
account id on this device, silently, and certifies this device with it.
The first run shows your **safety code** (behind the **?** on You), the fingerprint of that key, which
people compare with the code they see for you (see
[Identity & devices](../concepts/identity.md#safety-code)).

- **More devices** join your account from **Settings → Profile → My
  devices** (**Add a phone**, **Add a computer**). They don't sign in
  anywhere. A computer asks to join from the desktop app's first run (**I
  already use Ligus**), from `lt` (**Link to my devices**) or with `ligus
  devices request`. Don't invite yourself from another computer: that makes
  a second member. See [Add a computer](../features/linked-devices.md).
- **Commits** Ligus makes on your behalf are attributed to the commit email
  in your profile.
- **Reaching people** works through invite links and through the workspaces
  you share. See [People, contacts & DMs](../features/collaborators.md).

A GitHub, GitLab or Forgejo connection is only ever for repository work,
never for signing in.

## Reset local data

To start completely over on a device, use **Settings → Danger zone →
"Reset local data"**. It wipes this device's chats, identity, keys,
settings, and workspaces, then relaunches Ligus fresh.

This is the **supported way to start over** — uninstalling the app leaves
your data behind on disk. If you ever need to clean up manually, Ligus's
data directory is:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/ai.ligus.desktop` |
| Windows | `%APPDATA%\ai.ligus.desktop` (plus `%LOCALAPPDATA%\ai.ligus.desktop` for the WebView2 cache) |
| Linux | `~/.local/share/ai.ligus.desktop` |

!!! danger "Reset is irreversible"
    Reset local data cannot be undone. Anything not synced to a peer or
    pushed to git is gone. Back up your workspace folder first if in doubt.

## Next steps

- [Configure additional runtimes](configuring-a-runtime.md)
- [Start your first chat](first-chat.md)
- [Set up a relay so peers can find you](../../operate/networking/self-host.md)
