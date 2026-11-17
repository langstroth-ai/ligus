# Code hosts

A **code host** (or forge) is where your repositories live: GitHub, GitLab,
Codeberg, or your own Forgejo or Gitea server. Ligus connects to code hosts
for repository work only: listing repositories, cloning, and opening pull or
merge requests. **Ligus doesn't use a code host to sign you in.** Your Ligus
account is your own key, on your devices.

| Code host | Sign-in options | Draft change requests | Guide |
|---|---|---|---|
| GitHub (github.com and GitHub Enterprise Server) | the GitHub CLI login (`gh`) on the device, device sign-in through the Ligus GitHub App, or a fine-grained token | real draft pull requests | [GitHub](../forges/github.md) |
| GitLab (gitlab.com and self-managed) | device sign-in (gitlab.com, or an instance whose admin registered an application), or a personal access token | `Draft:` merge requests | [GitLab](../forges/gitlab.md) |
| Forgejo, Gitea, Codeberg | a personal access token | `WIP:` pull requests | [Forgejo, Gitea and Codeberg](../forges/forgejo-gitea-codeberg.md) |

Tangled and SourceHut repository work comes after 2.0. Bitbucket and Azure
DevOps are on request.

## Where tokens live

- **Only on the device that does the git work.** On the desktop, that's the
  desktop. For a [`ligus serve` box](../../operate/ops/serve-repos.md), it's the box. A
  phone never holds a code-host token: it drives the box.
- **Only in the OS secret store**: the Ligus keychain on macOS, the Secret
  Service on a Linux desktop, `systemd-creds` on a headless box, a mounted
  Secret on Kubernetes. Each token is stored as `forge:<connection>:<read|write>`.
- **Never** in `settings.json`, `forges.toml`, `hive.db`, a synced event, an
  invite, a log, or a request to your relay.
- **Never** on a command line or in an agent's environment. When Ligus runs
  `git push`, git asks Ligus's own credential helper, which reads the token
  from the secret store and answers only for that code host's own `https`
  address.

The list of connections is `forges.toml` in the data dir. It names each
code host and how it signs in, never a token. It is never synced.

## Read and write tokens

Each connection can hold two tokens:

- a **read** token, for listing repositories and for the agents' read-only
  tools;
- a **write** token, for pushing a branch and opening a change request.

Writes never use the read token. When a connection has only a write token
(or uses the `gh` login), Ligus reads with it on the device, but agents
still only get the read-only tools below.

## Agents read, people write

Agents can read the chat's code host through Ligus's own tools, in the Ligus
MCP server: pull or merge requests (with review comments and CI), issues, CI
status for a branch or commit, and a file at a ref. The tools run inside
Ligus, not in the agent's sandbox, so the token never reaches the agent.

- They reach only the chat's repository and, for a fork, its upstream.
- A chat whose egress is closed (a locked-down chat, or dearth mode) gets
  none of them. An allow-listed chat gets them only when its allow-list
  names `ligus.forge`.
- Each turn may make at most 16 code-host reads
  (`LIGUS_MCP_MAX_FORGE_CALLS`), on top of the usual tool budget.
- There is no write tool: no forks, no pull requests, no comments, no
  pushes. Pushes and change requests happen only when a person asks, from
  the desktop or the phone.

Community MCP servers (for example GitHub's own server with `--read-only`)
still work as your own MCP configuration.

## Self-hosted code hosts

Add a connection with your server's address, for example
`https://git.example.com` or `https://example.com/gitlab`. Ligus sends tokens
only over `https` (or to a server on the same machine). On a self-managed
GitLab, device sign-in needs an OAuth application on that server: see
[GitLab](../forges/gitlab.md#self-managed).

A server on a private, loopback or link-local address (or a name that
resolves to one) is refused unless you tick **Internal server on this device's network** when adding
it (`ligus forge add --internal` on a box). The mark stays on that device and
is never synced; it stops a code-host address from being used to reach
other machines on your network, such as a cloud metadata service. Behind an
HTTP proxy, the proxy resolves the address instead. That includes the proxy
in your macOS or Windows network settings, not only `HTTPS_PROXY`. A proxy on
your own machine or network is allowed from either source. A code-host
address with the same name as the proxy that `NO_PROXY` sends direct is
refused unless the connection is marked internal.

## On the desktop

**Settings → Code hosts** lists each connection: its address, how it signs in,
whether it has read and write tokens, and which device holds them. From
there you can:

- **Add** a code host (pick the kind, adjust the address for your own server).
- **Paste** a read or write token.
- **Sign in** with a device code where the code host supports it.
- **Test** the connection. It reports only whether signing in worked, never
  the server's reply.
- **Remove** it, which also deletes its tokens from the secret store.

Under **Repositories**, pick a connection, search, and **Clone…** a
repository into a folder; Ligus then offers it as the workspace folder.

## On the phone and the box

The phone manages the box's connections through the box. A token or a device
sign-in goes from the phone to the box only through
[remote access](../../operate/networking/remote-access.md), which is end-to-end
encrypted between the phone and the box. On a plain LAN or tailnet address
the box refuses them; set the token on the box itself instead:

```sh
ligus forge add gitlab https://gitlab.example.com
ligus forge token gitlab:gitlab.example.com write   # paste the token, then Enter
ligus forge test gitlab:gitlab.example.com
```

See [Repos on the home host](../../operate/ops/serve-repos.md) and the
[serve API](../../operate/reference/serve-api.md#code-hosts).
