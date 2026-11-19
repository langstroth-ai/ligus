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

### Aliases the workspace shares

Some names belong to the whole workspace rather than one device: the old
owner of a repository that moved (`github.com/old-org/wefit` for WeFit), a
different spelling on a case-sensitive server (`git.example.com/ops/infra`
for `git.example.com/Ops/Infra`), or the SSH host a self-hosted forge serves
from (`ssh.git.example.com` for `git.example.com`). An Admin can name them
once for everyone:

- **Repository aliases**: **Aliases…** on a repository in **Settings →
  Repositories**, or `ligus repos repo-alias <repo> <name>…`. At most eight
  per repository.
- **Host aliases**: the **Host aliases** section, or
  `ligus repos host-alias <alias-host> <repository-host>`. The target must be
  the host of a repository in the workspace. At most sixteen.

Each change needs an Admin's passkey (rules v2), like adding a repository.
Agents and phones can't make one.

Aliases are **names only**. They change how a device recognizes a checkout,
never where git connects: a clone always uses the repository's own canonical
name through this device's code-host connection, and an alias never carries a
scheme, user, port or address.

A shared alias changes nothing on a device until someone there accepts it:
**accept here** next to the alias, or `ligus repos accept <alias>`. Accepting
records what the alias points to (in Settings, what you saw: if an Admin
pointed it elsewhere in the meantime, accepting refuses and you look again),
so if an Admin later points it somewhere else, it stops counting here until
someone accepts it again; if the Admin removes it, it stops counting
everywhere, binding a checkout included. A checkout that matches only through
an alias shows "via alias", and a checkout found by a scan still needs
**Use it**.

Ligus refuses an alias that would make two repositories hard to tell apart:

- an alias that is already a repository's name or another repository's alias;
- a look-alike of either (Ligus compares names the same way it flags
  look-alike repositories: case, `0`/`o`, `1`/`l`/`i`/`|`, `5`/`s`, `8`/`b`,
  `rn`/`m` and `vv`/`w` folded and `.`, `-`, `_` ignored first, so `r-n`
  reads as `m` too), except a spelling that differs from the repository's
  own name only in upper and lower case;
- a host alias for a host a repository already uses, or a look-alike of one;
- a new repository whose name an alias already holds, or that sits on an
  aliased host.

When two Admins set clashing aliases at once, the one the log orders first
wins on every device and the other has no effect. Removing a repository drops
its aliases.

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
checkout from a host that is gone. Work that doesn't claim a checkout leaves
one another host manages alone too: the desktop's hygiene sweep skips it, and
a chat without a repository doesn't run in (or apply into) a workspace folder
that another host manages.

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
starts the chat from the same base. The first pin wins: a later pin from
another host or a member changes nothing, and pointing the chat at another
repository and back keeps the pin it had.

To move the base, an Admin or the chat's creator picks **Move the base to the
latest ‹branch›** in the chip (with your passkey under rules v2). Each host
with the chat's branch rebuilds it on the new base on its next turn: it
replays the chat's applied proposals there in order (leaving out reverted
ones, and skipping one the new base already contains) and moves the branch
only if it is still where it was. If it can't, the turn doesn't run and the
chat says why: a commit on the chat's branch that isn't an applied proposal
(Ligus never drops someone's work), local changes in the chat's folder, a
proposal that doesn't apply on the new base, or a new base that host hasn't
fetched.

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
instead; it is offered, and honoured, only when the chat has a repository,
the agent's host says it lacks it, this device has it ready, can run the
agent's runtime, and the agent is yours. It decides only what this device
does: nothing makes another member's device run a turn. The host backs off
when it sees this device's claim on the turn.

While a host is cloning the repository, a turn it has already claimed waits
for the clone (up to ten minutes). Coverage shown elsewhere is a hint: the
router skips an agent whose host says it lacks the chat's repository, and a
workflow names the stage whose host has none before it starts. Each host
signs its own coverage with its device key, and other devices use a report
only when the signature checks out against that host's certified device, so
no one can report coverage for a host that isn't theirs.

### Which device answers

When several devices could answer a turn (your laptop and your desktop for
your own agents, or a box and a desktop that both have the repository), they
settle it through the chat itself:

- Each device that may answer writes a signed claim. A device that has a
  confirmed checkout of the chat's repository says so in its claim, and a
  device without one waits a few seconds first, so a host that has the code
  claims first. A claim from a host with the code takes the turn from one
  without it.
- Every device reads the claims in the same order and agrees on one holder.
  Only the holder's answer counts: if two devices answered at once (for
  example during a network split), every device keeps the first and hides
  the other.
- If the holder goes quiet (no answer within about 25 seconds and no longer
  online), another device that may answer takes the turn over and answers
  it. A holder that stays online but hasn't started answering after two
  minutes is replaced too (one that has the repository only by another
  device that has it). A device that was revoked, or whose person left the
  workspace, loses its turns at once, and nothing it writes afterwards
  counts.
- A take-over only counts when it is signed at least 15 seconds after the
  claim it replaces, and an answer the holder wrote before it was replaced
  still counts, so a member's device can't slip in afterwards and take a
  turn that was answered promptly. A device holds one turn at most twice.
- Regenerating removes the answer and claims the turn again, so a host that
  sees the turn open again doesn't run it a second time. Only the answer's
  author or an Admin can remove an answer.
- Hidden duplicates stay out of everything: the chat on the desktop and
  phone, notifications, the next turn's context, summaries, exports, agents'
  read tools and workflow stage outputs.
- A device on its own (no relay, or a workspace with only you in it) always
  answers its own turns.
- While devices run mixed versions, an older build reads a hand-over as a
  plain claim and keeps the first holder, so it may show a duplicate answer
  the newer devices hide (design notes:
  `docs/design/v2/repos-and-checkouts.md`, section 11.6).

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
  changing the branch) and the branch's current tip. The conflict is quoted
  as data in a block nothing in it can close, and the proposal's title stays
  on one line, so text a patch chose never reads as your words. The agent's
  new version supersedes this one and needs approval again; an edited result
  never enters the chat's history without a vote.
- **Open in editor**: opens the chat's folder for the repository with your
  system's default handler for folders, to look at the conflict or at local
  changes in the way. Only a real folder inside the checkout is opened (never
  a link, which a repository could plant there), and on macOS never an app or
  other package the opener would launch; no shell is involved.

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
