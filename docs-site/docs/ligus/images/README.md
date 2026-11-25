# Screenshots & diagrams

The screenshots in these docs are of real Ligus apps running on **demo
data**: the desktop app's fixture backend, lt's demo backend and the phone
app's demo workspace. No real workspace, person, host or key appears in
them. They were captured from **Ligus v0.3.0** and **lt v0.3.0**.

[`CAPTURE_GUIDE.md`](CAPTURE_GUIDE.md) says how to recapture them.

## Naming

One file per view and appearance, Pollen palette:

- `desktop-<view>-<light|dark>.png`: the desktop app
- `lt-<view>-<light|dark>.png`: lt, the terminal app
- `phone-<view>-<light|dark>.png`: the iPhone app

Pages embed both variants with Material's `#only-light` and `#only-dark`,
so each reader sees the one that matches the docs' colour scheme:

```markdown
![Alt text](../images/desktop-inbox-light.png#only-light){ width="760" loading=lazy }
![Alt text](../images/desktop-inbox-dark.png#only-dark){ width="760" loading=lazy }
```

## Where each is used

| Files | What it shows | Used by |
|---|---|---|
| `desktop-overview-*` | Full window: a chat with agents handing work to each other, the Review pane with a proposal, its diff and votes | `index.md`, `ligus/index.md` |
| `desktop-onboarding-identity-*` | Onboarding step 1: name and safety code | `getting-started/first-launch.md`, `features/onboarding.md` |
| `desktop-onboarding-agent-*` | Onboarding: choose your agent | `getting-started/first-launch.md` |
| `desktop-onboarding-team-*` | Onboarding: Team & sync | `features/onboarding.md` |
| `desktop-onboarding-appearance-*` | Onboarding: palette and mode | `getting-started/first-launch.md` |
| `desktop-inbox-*` | The Inbox with every kind of item | `features/inbox.md` |
| `desktop-review-*` | Review pane: a proposal, its diff, one of two approvals | `features/right-rail.md` |
| `desktop-tools-*` | Agents & tools pane | `features/right-rail.md` |
| `desktop-invite-*` | People pane, Invite someone | `features/invites.md` |
| `desktop-settings-profile-*` | Settings, Profile: linked accounts, My devices | `features/settings.md`, `features/linked-devices.md` |
| `desktop-settings-models-*` | Settings, Models & runtimes | `getting-started/configuring-a-runtime.md` |
| `desktop-settings-tools-*` | Settings, Tools & MCP | `addons/mcp.md` |
| `desktop-appearance-*` | Settings, Appearance: mode, palettes, theme files | `features/themes.md` |
| `lt-chats-*` | lt: the chat list and a chat with proposals | `features/themes.md` |
| `phone-chats-*` | Phone: the Chats tab | `features/phone-app.md` |
| `phone-proposal-*` | Phone: a proposal with votes and its diff | `features/phone-app.md` |

To list every image reference:

```bash
grep -rn '](.*images/' docs-site/docs --include='*.md'
```

## Conventions

- **Demo data only.** No real names, emails (use `example.com`), hostnames,
  room ids, tokens or invite codes.
- **Pollen**, light and dark. Window shots at 1440 × 900 at 2×, scaled to
  2160 px wide; crops of a pane or dialog stay at 2×.
- **PNG, compressed** with `pngquant --quality=65-90`. Keep each file under
  about 250 KB.
- **Alt text** says what the shot shows, and "(demo data)".
