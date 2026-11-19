# ligus serve: the home host for the iOS app

`ligus serve` runs on an always-on Linux box on your tailnet. It is an ordinary
Ligus device in your workspace: it syncs the E2EE event log through the relay
like the desktop app, runs agents you send to it on your own model gateway, and
gives the iPhone app a small HTTP + Server-Sent Events API on the box's
Tailscale address. The phone holds only a bearer token. The workspace key and
the device keys stay on the box. Design and API contract: [`docs/ios-app.md`](../../docs/ios-app.md).

[`deploy/local/ligus-local`](../local/ligus-local) scripts steps 2 and 5 below
(configure, start, verify, stop) for a person or a coding agent; see
[`AGENTS.md`](../../AGENTS.md). Joining the workspace (step 3) stays yours.

Files here:

| File | Goes to |
|---|---|
| `ligus-serve.service` | `~/.config/systemd/user/ligus-serve.service` |
| `ligus-serve.env.example` | `~/.config/ligus-serve/ligus-serve.env` (mode 0600, then edit) |
| `runtimes.toml.example` | `$LIGUS_DATA_DIR/runtimes.toml` |
| `mcp.toml.example` | `~/.config/ligus-serve/mcp.toml` (optional) |
| `dashboard.toml.example` | `$LIGUS_DATA_DIR/dashboard.toml` (optional: the app's dashboard tab, [`docs/dashboard.md`](../../docs/dashboard.md)) |
| `dashboard-solar.toml.example` | append to `dashboard.toml` (optional solar section) |
| `dashboard-team.toml.example` | `$LIGUS_DATA_DIR/dashboard.toml` (optional: a dashboard without a home lab: workspace metrics, an MCP-backed table, a health check) |

## 1. Build on the box

```sh
# Rust toolchain, if the box has none (installs to ~/.cargo, no root needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
sudo apt-get install -y build-essential pkg-config git   # C toolchain for bundled SQLite

git clone https://github.com/langstroth-ai/ligus.git ~/src/ligus   # or pull your branch
cd ~/src/ligus
cargo build --release -p ligus-cli
install -Dm755 target/release/ligus ~/.local/bin/ligus
```

Only the `ligus-cli` crate (it builds the `ligus` binary) builds. The Tauri app and its web UI aren't needed on
the box.

## 2. Configure

```sh
install -Dm600 deploy/ligus-serve/ligus-serve.env.example ~/.config/ligus-serve/ligus-serve.env
$EDITOR ~/.config/ligus-serve/ligus-serve.env        # LIGUS_DATA_DIR, LIGUS_SERVE_LISTEN, ntfy, ...
tailscale ip -4                                     # the address for LIGUS_SERVE_LISTEN
```

For the CLI commands below, load the same environment into your shell:

```sh
set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a
```

## 3. Join the workspace

You run this step yourself. It needs your workspace invite, and that invite
carries the workspace's E2EE key.

1. In the desktop app, open the workspace and use **Copy invite**. It gives you
   a `hivews1:…` code. Send it to the box over a channel you trust, such as
   SSH. Don't use chat or email.
2. On the box:

   ```sh
   ligus enroll -                            # paste the invite, then Ctrl-D; add --token <hrt1…> if your relay needs an access token
   ligus join <invite-code>                  # only on relays that enforce membership
   ligus sync                                # pull the workspace
   ligus whoami                              # this box's account/device ids
   ```

`enroll` writes the connection, including the key, to
`$LIGUS_DATA_DIR/config.toml`. Keep that directory private (the unit sets
`UMask=0077`).

On first start, `ligus serve` adds itself to the roster at the **Contributor**
role. That is enough to chat, vote on proposals and move agents between hosts.
It also registers itself as a host, so it appears in the desktop's host list
under `LIGUS_SERVE_LABEL`. A proposal whose `approvalRoleFloor` sits above
Contributor needs an Owner/Admin to promote this member in the desktop's
People pane before the phone's vote counts. Defining workspace runtimes
(`ligus add-runtime`) needs Admin.

## 4. Models, repos and tools

- **Runtimes.** Copy `runtimes.toml.example` to `$LIGUS_DATA_DIR/runtimes.toml`
  and point it at your gateway. Agents you send to this host use the runtime
  listed there (the `*` entry catches all), unless a workspace runtime with the
  agent's runtime id exists.
- **Repos.** Clone the checkouts agents should work on under `LIGUS_REPOS_DIR`
  and bind one:

  ```sh
  git clone <url> "$LIGUS_REPOS_DIR/ligus"
  ligus bind-repo ligus                  # the workspace's checkout
  ligus bind-repo other --chat <id>     # one chat on a different checkout
  ligus repos                           # show bindings
  ```

  The binding stays on this box and never syncs, the same as the desktop's
  workspace folder. A turn reads the chat's base worktree
  (`.ligus/worktrees/chat-<id>`). Its first write cuts a per-turn worktree, and
  the diff comes back as a review proposal. Nothing is written to the checkout
  itself. Turns in a chat with no bound checkout get no file tools.
- **MCP.** `LIGUS_MCP_CONFIG` names a TOML file of MCP servers (see
  `mcp.toml.example`). Bearer tokens come from the environment variables it
  names.
- `run_command` is off unless `LIGUS_BUILTIN_RUN_COMMAND=1`.

## 5. Run

```sh
install -Dm644 deploy/ligus-serve/ligus-serve.service ~/.config/systemd/user/ligus-serve.service
systemctl --user daemon-reload
systemctl --user enable --now ligus-serve
sudo loginctl enable-linger "$USER"     # start at boot, keep running after logout
journalctl --user -u ligus-serve -f
```

Health check from any tailnet device: `curl http://<tailscale-ip>:8787/v1/health`.

## 6. Pair the phone

```sh
# Load the unit's settings first: without them `ligus pair` has no listen
# address or data dir, and refuses rather than print a loopback link.
set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a
ligus pair        # prints a QR code and a ligus://pair?... link for the configured address
```

Scan the QR code with the iPhone camera, or paste the link into the app. The
link contains the API token, so run `ligus pair` in your own terminal. Don't run
it in the unit, because the journal would keep the token. To revoke the phone,
delete `$LIGUS_DATA_DIR/serve.token` and restart. A new token is generated, and
you pair again.

Optional, remote access without Tailscale: `ligus remote enable --relay
https://<relay>` (with the unit's env loaded), restart the unit, then `ligus
pair` prints a v2 link that also pairs over the relay. Phones paired that way
are listed by `ligus phones list` and revoked with `ligus phones revoke <name>`.
See [`docs/ios-app.md`](../../docs/ios-app.md#remote-access-through-a-relay-optional-off-by-default).

## Sending agents here

From the phone, open **Agents**, pick an agent and choose this host
(`PUT /v1/agents/{id}/host`). From a terminal, run
`ligus set-agent-host <agent> <host-id>`. After that, any `@agent` mention in
the workspace runs on this box. The desktop shows the agent's host as this
box's label.
