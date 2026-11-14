# Forgejo, Gitea and Codeberg

Ligus treats Forgejo, Gitea and Codeberg (which runs Forgejo) as one kind of
code host. A connection is for repository work only; it never signs you in
to Ligus.

## Connect with a token

These servers have no device sign-in, and Forgejo's OAuth tokens carry no
scopes (they can do anything you can), so Ligus uses a scoped personal
access token.

1. On the server: **Settings → Applications → Generate new token**.
2. Give it the scopes below. On Forgejo you can limit it to specific
   repositories.
3. Paste it in **Settings → Code hosts** (or `ligus forge token
   gitea:<host> write` on a box).

| Token | Scopes |
|---|---|
| Read | `read:repository`, `read:issue` |
| Write | `write:repository`, `write:issue` |

## Pull requests

- Pull requests have no draft flag here, so a draft is titled `WIP: …`
  (one of the default work-in-progress prefixes).
- From a fork, Ligus pushes your branch to the fork and opens the pull
  request on the parent as `you:branch`. It never pushes to the parent.
- If one is already open for that branch, the push updates it.

## Your own server

Add a connection with your server's address, for example
`https://git.example.com`; Ligus uses `/api/v1` under it. Pick **Forgejo** or
**Gitea**: both use the same API. Codeberg is `https://codeberg.org`.

CI status reads commit statuses, which Forgejo Actions and Gitea Actions
report.
