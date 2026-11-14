# GitLab

Ligus works with gitlab.com and self-managed GitLab. A GitLab connection is
for repository work only; it never signs you in to Ligus.

## Ways to connect

1. **Device sign-in.** In **Settings → Code hosts**, choose **Sign in**, then
   enter the code on the GitLab page. On gitlab.com this uses the Ligus
   application. On a self-managed instance it needs an application on that
   instance (below). Device sign-in needs GitLab 17.9 or later (17.3 or
   later when the `oauth2_device_grant_flow` flag is on).
2. **A personal access token**, pasted in **Settings → Code hosts** (or
   `ligus forge token gitlab:<host> write` on a box).

## Scopes

| Token | Scopes |
|---|---|
| Read | `read_api`, `read_repository` |
| Write | `api`, `write_repository` |

`api` is needed to open merge requests; `write_repository` to push the
chat's branch over HTTPS. A fine-grained personal access token (GitLab 19.2
and later) works too: give it the same reach on the projects you choose.
Device sign-in asks for `api read_repository write_repository`.

## Merge requests

- GitLab's API has no draft flag when a merge request is created, so a draft
  is titled `Draft: …`.
- From a fork, Ligus creates the merge request on your fork with the parent
  as the target project. It never pushes to the parent.
- If a merge request for that branch is already open, the push updates it.
- Nested groups work: `group/subgroup/project`.

## Self-managed

Add a connection with your instance's address, for example
`https://gitlab.example.com`. Ligus uses `/api/v4` under it.

For device sign-in, an admin (or you, as a user application) registers an
application: **Admin → Applications** (or **User settings → Applications**):

- **Name**: Ligus.
- **Redirect URI**: any value the form accepts (device sign-in doesn't use
  it), for example `http://127.0.0.1/`.
- **Confidential**: off.
- **Scopes**: `api`, `read_repository`, `write_repository`.

Put the **Application ID** on the connection (**Application id** field in
Settings → Code hosts, or `ligus forge add gitlab <url> --client-id <id>`).

## For maintainers: the gitlab.com application

Owner action, once: register the application above on gitlab.com under the
Ligus group (**Group → Settings → Applications**), non-confidential, with
**Device authorization grant** allowed. Bake its Application ID into release
builds with `LIGUS_GITLAB_CLIENT_ID=<id>` at build time.
