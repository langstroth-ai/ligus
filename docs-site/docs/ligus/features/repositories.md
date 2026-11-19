# Workspace repositories

A workspace can name the repositories it works on, for example
`github.com/zazulam/wefit`. Every member's devices, your `ligus serve` box and
your phone then agree on which code a chat is about, which hosts have it
checked out, and where a turn may run.

Two things are kept apart:

| | What it is | Where it lives | Synced |
|---|---|---|---|
| **Workspace repository** | A name such as WeFit and a canonical identity such as `github.com/zazulam/wefit`, with a default-branch hint. | A signed event on the workspace's config log. | Yes, to every member |
| **Checkout** | "On this device, WeFit is at `~/apps/WeFit`." | This device's own database. | **Never** |
| **Coverage** | "This host has WeFit checked out" (or is cloning it, or doesn't). No paths. | The host's encrypted presence beat. | Yes, while the host is online |
| **Chat repository** | "This chat works in WeFit, on `main`." | A signed event on the chat. | Yes |

Paths never leave a device. Other hosts show only by name, with whether they
have each repository checked out.

## Add a repository

Adding or removing a repository is an Admin's act, confirmed with a passkey
(rules v2). Renaming it or changing its default branch needs no passkey.

- **From a folder you bound.** Link a folder to the workspace (**Choose
  folder…**). If its remote isn't a workspace repository yet, an Admin sees
  "This folder is github.com/zazulam/wefit. Add WeFit to the workspace?"
  **Add** adds it and records the folder as this device's checkout. Others see
  "Ask an admin to add WeFit".
- **In Settings → Repositories**, paste a remote URL in any git form or a
  canonical name (`github.com/owner/name`).
- **On a box**: `ligus repos add <remote | canonical | path>`.

A repository's id is derived from its canonical name, so two Admins adding the
same repository from two devices end up with one. Adding a repository moves no
bytes: no device fetches anything until a person clones it or binds a folder.

### How a remote becomes an identity

Ligus turns a remote into its canonical form with one fixed rule, the same on
every device:

- `git@github.com:ZaZulam/WeFit.git`, `ssh://git@github.com:22/zazulam/wefit`
  and `https://www.github.com/zazulam/wefit/` are all
  `github.com/zazulam/wefit`. Userinfo, port, `www.`, a trailing `.git` and
  slashes are dropped; `ssh.github.com` is `github.com`.
- Owner and name are lowercased on github.com, gitlab.com, codeberg.org and
  gitea.com, and on any host the entry names as GitHub, GitLab, Gitea or
  Forgejo. On other hosts the case is kept: two spellings stay two entries
  rather than two different repositories merging into one.
- Hosts must be ASCII (write an international host in punycode). Spaces,
  `%` in the path, `\`, `?` and `#` anywhere, `..` segments, IP-literal hosts
  and git transports such as `ext::` are refused.
- Remotes are read from the checkout's own config, without includes and
  without `insteadOf` rewrites: a rewrite changes where git connects, never
  what the repository is. A remote with more than one URL matches nothing.
- A repository with no forge remote is `local:<root commit>` and matches a
  checkout that contains that root commit.

If you reach a forge through an SSH alias (`Host github-work` in
`~/.ssh/config`), tell this device what the alias means:
`ligus repos alias github-work github.com`. The alias map stays on the device.

### Forks

A fork is not a separate repository. A checkout covers WeFit when its
`origin` is WeFit, or when its `upstream` is (your own fork as `origin`, the
workspace repository as `upstream`). Coverage then says "via a fork", never
which fork. Pushes still go to your fork.

## Checkouts on this device

**Settings → Repositories** lists each repository with:

- this device's checkout (its path, shown only here), **Bind folder…** and
  **Unbind**;
- checkouts a scan found, each with **Use it**: a found checkout is never used
  until a person confirms it;
- every other host's coverage: checked out, cloning, not checked out, or
  unknown (a host that doesn't report).

**Clone into…** clones a repository this device doesn't have into a folder
you pick, as `<name>-<first 8 characters of its id>` (for example
`wefit-1a2b3c4d`), and records it as this device's checkout. It takes the same
path as a box's clone by id: the URL is rebuilt from the repository's
canonical name for a code host connected under **Settings → Code hosts** (with
no connection to that host, it is refused), the host's address is checked
again and pinned for git, credentials go through the connection's helper and
never on a command line, submodules are not fetched, and the clone appears
only once it is complete. If the clone doesn't contain the first commit the
workspace entry names, Ligus says so. Cloning on another host (a box, a
worker) is done from the phone connected to it, never from the desktop.

**Scan a folder…** looks for checkouts under a folder (at most three levels
deep, 2,000 folders, no symlinks; never your home folder itself or a system
folder). **Rescan** repeats it for the folders you added.

A checkout has one **managing host** on a machine: the first Ligus process
to use it writes a marker in its git directory. When the desktop app and a
`ligus serve` box run on the same Mac, the box refuses the desktop's folder and
keeps its own clone. On a box, `ligus repos takeover <path>` takes over a
checkout from a host that is gone.

**Tell other members which of these repositories this device has checked
out** is on by default in workspaces you own and off elsewhere. With it off,
others see "unknown", not "not checked out".

## A chat's repository

The chip at the top of a chat shows its repository and base branch ("WeFit ·
main") or "No repository". Click it to pick another workspace repository, or
none. Contributors and above can change it; agents can't.

A new chat branch, `hive/chat/<chat-id>`, is cut from the chat's base branch
(`upstream/<branch>` in a fork, else `origin/<branch>`, else the local branch).
The first host to cut it **pins** that commit for the chat, so every device
starts the chat from the same base.

### Where a turn runs

The host that runs a turn is the only judge of whether it can:

- a chat without a repository runs as before;
- a chat with one runs only in this host's confirmed checkout of it, whose
  remote is still that repository, and that this host manages;
- otherwise the turn doesn't run, and the chat says why: "No checkout of
  WeFit on mac-test. Clone it here or bind a folder." A turn never runs in
  other code.

A turn also takes everything else from its own chat, not from the chat or
folder the window shows: it runs in its chat's folder (for a chat with a
repository, its chat branch in this device's checkout; otherwise its chat
branch in the workspace's folder), and its runtimes, MCP servers and trusted
items are read from that folder. A server a repository supplies is offered to
a turn only in a chat on that repository, and only after you trust it for that
repository: trusting a server in one repository doesn't trust the same server
in another. The window's file tree and file watcher follow the chat you are
viewing; viewing a chat never cuts its branch.

If a chat's agent is pinned to a host that says it lacks the chat's
repository, the composer says so when you address it: "ScoutBee runs on
mac-test, which has no WeFit. Clone there · Run here this time". **Clone
there** tells you how to clone it on that host (from the phone connected to
it). **Run here this time** sends that one message's turn to this device
instead; it is offered only when this device has the repository, can run the
agent's runtime, and the agent is yours. The host backs off when it sees this
device's claim on the turn.

While a host is cloning the repository, a turn it has already claimed waits
for the clone (up to ten minutes). Coverage shown elsewhere is a hint: the
router skips an agent whose host says it lacks the chat's repository, and a
workflow names the stage whose host has none before it starts.

## Applying proposals across devices

A proposal records the repository it is for (taken from the chat when it is
filed, and never changed after), the commit it was made on, and the host that
made it. The vote covers all three, so an approval for a change to WeFit is
never an approval for Infra.

**Implement** applies a proposal only in this device's checkout of its
repository. Ligus builds the commit in a scratch index (or a scratch worktree
for a three-way merge) and moves the chat's branch only if it is still where it
was: a conflict changes nothing, and the chat says so. A proposal made on
another base that merges cleanly is applied and noted as "applied on a
different base".

When a proposal doesn't apply, the card offers:

- **Ask the agent to rebase**: files a follow-up turn to the agent that made
  it, with the conflict as this device's git reports it (found without
  changing the branch) and the branch's current tip. The agent's new version
  supersedes this one and needs approval again; an edited result never enters
  the chat's history without a vote.
- **Open in editor**: opens the chat's folder for the repository with your
  system's default handler for folders, to look at the conflict or at local
  changes in the way.

Every apply commit carries `Ligus-Proposal: <id>` in its trailer block.
Before a turn, a host replays any applied proposal its chat branch lacks (by
that trailer, on commits since the chat's pinned base), in the order the chat
applied them, and takes back out any that were reverted. If a replay
conflicts, the turn doesn't run and the chat says the branch has diverged. A
host whose chat branch wasn't cut from the chat's pinned base (another host
pinned first) refuses the chat's turns the same way: start a new chat, or
remove that host's chat branch so it is cut again from the pinned base.

## Privacy

- The list of repositories is visible to every member of the workspace,
  including Viewers. The relay sees none of it.
- Coverage says which repositories a host has, and whether through a fork. It
  carries no path, remote or branch.
- Phone and API responses carry checkout folder names, never paths.

See also: [Repos on the home host](../../operate/ops/serve-repos.md),
[Git integration](git.md), [Code hosts](code-hosts.md).
