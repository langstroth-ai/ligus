# Repos on the home host

The agents on your `hive serve` box work on git checkouts on that box. From
the [phone](../features/phone-app.md) you can pick which repository a chat
works in, clone a new one from GitHub, pull the latest, and open a pull
request, all without leaving the chat.

## Where checkouts live

Checkouts live under `HIVE_REPOS_DIR` (default `<HIVE_DATA_DIR>/repos`). The
workspace has a default checkout, and any chat can override it. Both are kept
in `<HIVE_DATA_DIR>/repo-bindings.toml` on the box and are never synced:

```toml
workspace = "sandbox"
[chats]
"6f1c…-chat-id" = "honeyhive-ai__hive"
```

You can set the default from the box's shell with
`hive bind-repo <name>`. A repo cloned from the phone is named
`<owner>__<name>`.

## Sign the box in to GitHub (once)

Listing your GitHub repos, cloning private ones, and opening pull requests use
the box's own GitHub CLI login. Hive never stores a GitHub token. On the box:

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

- **Switch** the chat to another checkout. The chat stays the same. Hive posts
  a note in the chat, and the agent's next turn is told which repository and
  branch it's working in.
- **Clone** a GitHub repo. The clone runs on the box with progress, and once
  it finishes you can switch to it.
- **Pull latest** fast-forwards the checkout's base branch from `origin`. A
  checkout with local changes, or one that has diverged, is left alone.
- **Open pull request** pushes the chat's branch and opens a PR against the
  default branch. The link is posted in the chat.

## Branches: one per chat, per repo

Every chat has its own branch, `hive/chat/<chat-id>`, in each repo it works
in. An agent's edits come back as a proposal. When you approve it, the box
commits it to that branch, as long as `HIVE_SERVE_APPLY_ON_APPROVE=1` is set
(see [Running hive serve as your own device](serve-box.md)). Because the
branch is per repo, moving a chat to another repo and back resumes where it
left off.

Pull requests are opened from that branch. Hive never force-pushes. If the
remote branch has moved, the push is refused and the phone says so.

## Limits and safety

- Only `github.com` repositories, given as `owner/name` or
  `https://github.com/owner/name`, can be cloned. Other hosts, paths and URL
  forms are refused.
- A clone stops after `HIVE_REPOS_CLONE_TIMEOUT_SECS` (default 900) or once
  it passes `HIVE_REPOS_MAX_CLONE_MB` (default 2048), and leaves nothing
  behind. Two clones can run at a time.
- Changing repos needs the phone's pairing, a box that can write, and at
  least the Contributor role in the workspace.
- The phone never sees paths on the box, only checkout names.

The API is documented in `docs/ios-app.md` in the Hive repository.
