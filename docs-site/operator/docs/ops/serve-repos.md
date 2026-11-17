# Repos on the home host

The agents on your `ligus serve` box work on git checkouts on that box. From
the [phone](../features/phone-app.md) you can pick which repository a chat
works in, clone a new one from GitHub, GitLab, Codeberg, Forgejo or Gitea,
pull the latest, and open a pull or merge request, all without leaving the
chat.

## Where checkouts live

Checkouts live under `LIGUS_REPOS_DIR` (default `<LIGUS_DATA_DIR>/repos`). The
workspace has a default checkout, and any chat can override it. Both are kept
in `<LIGUS_DATA_DIR>/repo-bindings.toml` on the box and are never synced:

```toml
workspace = "sandbox"
[chats]
"6f1c…-chat-id" = "honeyhive-ai__hive"
```

You can set the default from the box's shell with
`ligus bind-repo <name>`. A repo cloned from the phone is named
`<owner>__<name>` (from github.com) or `<host>__<owner>__<name>` (from any
other code host; GitLab subgroups are joined with `__`).

## Connect the box to your code hosts (once)

Listing your repos, cloning private ones, and opening pull or merge requests
use the box's own code-host connections. Tokens stay in the box's secret
store; see [Code hosts](../features/code-hosts.md). Other code hosts:

```sh
ligus forge add gitlab https://gitlab.example.com
ligus forge token gitlab:gitlab.example.com write   # paste the token on stdin
ligus forge add codeberg https://codeberg.org
ligus forge token gitea:codeberg.org write
ligus forge list
```

You can also add a code host and paste its token from the phone, through
[remote access](../networking/remote-access.md) (end-to-end encrypted to the
box). The box refuses tokens over a plain LAN or tailnet address.

### GitHub through the box's gh login

By default the box's GitHub connection uses its own GitHub CLI login, and
Ligus doesn't store that token. On the box:

```sh
gh auth login -h github.com -p https -s repo,read:org --web
```

It prints a one-time code. Open `https://github.com/login/device` on any device
(your phone works), enter the code, and approve. Check it with
`gh auth status`. Until the box is signed in, the phone shows **Sign in on the
host**, still lists the box's checkouts, and can still clone public repos.

## From the phone

Tap the repo chip in a chat's header (or its details sheet) to see the bound
repo and branch, then:

- **Switch** the chat to another checkout. The chat stays the same. Ligus posts
  a note in the chat, and the agent's next turn is told which repository and
  branch it's working in.
- **Clone** a repo from any connected code host. The clone runs on the box
  with progress, and once it finishes you can switch to it.
- **Pull latest** fast-forwards the checkout's base branch from `origin`
  (from `upstream` in a fork). A checkout with local changes, or one that has
  diverged, is left alone.
- **Open pull request** pushes the chat's branch and opens a pull request
  (a merge request on GitLab) against the default branch (upstream's, as a
  draft, in a fork). The link is posted in the chat.

## Forks: contributing upstream

If you contribute to a project through your own fork (you push to
`you/project`, and pull requests go to `org/project`), the box handles the
fork layout for you:

- **Clone via my fork.** When you pick a repository you don't own, the phone
  asks the box whether your account has a fork of it. If it does, it offers to
  clone the fork instead. The box never creates a fork; if you don't have one,
  fork it on GitHub first.
- **`upstream` is set up for you.** Whenever a cloned or fetched repository
  is a fork on GitHub, the box adds an `upstream` remote for the parent. The
  remote is fetch-only: its push URL is a dummy, so nothing can be pushed to
  the parent. The checkout's base branch tracks `upstream`'s default branch,
  for example `master` tracks `upstream/master`.
- **Chats start from upstream.** New chat branches are cut from
  `upstream/<default>`, not your fork's possibly stale copy.
- **Pull latest** fetches `upstream` and fast-forwards the base branch from
  it.
- **Open pull request** pushes the chat's branch to your fork with a readable
  name, `hive/<chat-title>-<chat-id-prefix>`, and opens a draft pull request
  against the parent's default branch. Later pushes from the same chat update
  the same branch and pull request, and they are never forced. A push whose
  remote points at the parent project is refused.

The phone shows a fork as "fork of org/project · PRs open upstream". On the
box, `ligus repos` lists each fork checkout and whether its `upstream` push is
disabled.

To set up an existing checkout by hand:

```sh
git remote add upstream https://github.com/org/project.git
git remote set-url --push upstream DISABLED-push-to-fork-only
git fetch upstream
git branch -u upstream/master master
```

## Branches: one per chat, per repo

Every chat has its own branch, `hive/chat/<chat-id>`, in each repo it works
in. An agent's edits come back as a proposal. When you approve it, the box
commits it to that branch, as long as `LIGUS_SERVE_APPLY_ON_APPROVE=1` is set
(see [Running ligus serve as your own device](serve-box.md)). Because the
branch is per repo, moving a chat to another repo and back resumes where it
left off.

Pull requests are opened from that branch. Ligus never force-pushes. If the
remote branch has moved, the push is refused and the phone says so.

## Limits and safety

- A repository is given as `owner/name` (on the chosen connection, by default
  github.com) or `https://<host>/owner/name` on a connected code host.
  Unconnected hosts, credentials in URLs, other schemes, paths and queries are
  refused.
- A clone stops after `LIGUS_REPOS_CLONE_TIMEOUT_SECS` (default 900) or once
  it passes `LIGUS_REPOS_MAX_CLONE_MB` (default 2048), and leaves nothing
  behind. Two clones can run at a time.
- Changing repos needs the phone's pairing, a box that can write, and at
  least the Contributor role in the workspace.
- The phone never sees paths on the box, only checkout names.

The API is documented in `docs/ios-app.md` in the Ligus repository.
