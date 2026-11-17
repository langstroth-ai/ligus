# GitHub

Ligus works with github.com and GitHub Enterprise Server. A GitHub
connection is for repository work only; it never signs you in to Ligus.

## Ways to connect

Pick one per device or box.

1. **The GitHub CLI login (`gh`) on the device.** This is the default on a
   `ligus serve` box. Ligus asks `gh auth token --hostname github.com` when a
   request needs a token, and git uses `gh auth git-credential`. Ligus doesn't
   store this token.

    ```sh
    gh auth login -h github.com -p https -s repo,read:org --web
    ```

2. **Device sign-in through the Ligus GitHub App.** In **Settings → Code
   hosts**, choose **Sign in**, then enter the code GitHub shows. Check that
   the page names the Ligus app before you approve. The app's permissions are
   fine-grained (below), and its tokens expire after 8 hours and refresh on
   their own. This needs a Ligus build with the app's client id; without one,
   the button isn't shown.
3. **A fine-grained personal access token**, pasted in **Settings → Code
   hosts** (or `ligus forge token github:github.com write` on a box).

## Permissions

A fine-grained token, or what the Ligus GitHub App asks for, on the
repositories you choose:

| Permission | Level | Used for |
|---|---|---|
| Metadata | Read | always required |
| Contents | Read and write | clone, push the chat's branch |
| Pull requests | Read and write | list, read, open draft pull requests |
| Issues | Read | the agents' issue reads |
| Commit statuses | Read | CI status |
| Checks | Read | check runs (GitHub App only) |

For a **read token**, give every permission above as Read only.

Fine-grained personal access tokens can't read the Checks API. With one,
CI status shows commit statuses only and says
"checks unavailable with this token".

Ligus never creates a fork for you, so it doesn't need the Administration
or Repository creation permissions.

## Pull requests

- From a fork, Ligus pushes a readable branch to your fork and opens a
  **draft** pull request on the parent project. It never pushes to the parent.
- Where draft pull requests aren't available on the repository's plan, Ligus
  opens a normal one and says so.
- If a pull request for that branch is already open, the push updates it.

## GitHub Enterprise Server

Add a connection with your server's address, for example
`https://github.example.com`. Ligus uses `https://github.example.com/api/v3`.
For device sign-in, register a GitHub App on your server (as below) and put
its client id on the connection.

## For maintainers: registering the Ligus GitHub App

Owner action, once per Ligus distribution. On GitHub, **Settings →
Developer settings → GitHub Apps → New GitHub App**:

- **Name**: Ligus (short for *ligustica*). **Homepage URL**: `https://ligus.ai`.
- **Callback URL**: leave empty. Device sign-in doesn't use one.
- **Expire user authorization tokens**: on (8-hour tokens, 6-month refresh
  tokens; a device-flow token refreshes without a client secret).
- **Request user authorization (OAuth) during installation**: off.
- **Enable Device Flow**: on.
- **Webhook**: off (clear **Active**).
- **Repository permissions**: as in the table above (Contents read and
  write, Pull requests read and write, Issues read, Commit statuses read,
  Checks read, Metadata read). No organization or account permissions.
- **Where can this GitHub App be installed?**: any account.

Bake the app's **client id** (not the client secret, which isn't used) into
release builds with `LIGUS_GITHUB_APP_CLIENT_ID=<client id>` at build time,
or set it at run time on a box.
