# Documentation site

The published docs are one MkDocs (Material) site built from this directory
and served at **<https://docs.langstroth.ai/>**. It has two sections:

| Section | Directory | URL prefix | Audience |
|---|---|---|---|
| Using Ligus | `docs/ligus/` | <https://docs.langstroth.ai/ligus/> | People using the Ligus desktop app, phone app and `ligus` CLI |
| Operating Langstroth | `docs/operate/` | <https://docs.langstroth.ai/operate/> | People self-hosting and deploying: relay, langstroth-auth, Helm charts, `ligus serve` and worker hosts, security model, operator reference |

*Ligus* is short for *ligustica*, the Italian honeybee, and is the app;
Langstroth is the platform behind it. The home page (`docs/index.md`)
introduces both and links to each section. Developer and maintainer docs are
in [`../docs/`](../docs/README.md), not here.

```
docs-site/
  mkdocs.yml          the site: theme, Markdown extensions, nav, redirect stubs
  netlify.toml        build, headers and the 301s (Netlify base directory)
  requirements.txt    pinned build dependencies
  theme/              logo, favicon and stylesheet (theme custom_dir)
  docs/index.md       the home page
  docs/ligus/         Using Ligus
  docs/operate/       Operating Langstroth
  docs/llms.txt       every page with a one-line summary, for agents
  docs/version.json   the desktop app's update manifest
  tools/page-map.tsv  every page of the older sites and where it lives now
  tools/redirects.py  generates and checks the redirects
```

## Which section a page belongs in

Using the apps (workspaces, agents and runtimes, people and DMs, workflows,
proposals, passkeys as users see them, the CLI basics, what's new) goes in
`docs/ligus/`. Running the platform (deployment shapes, the relay and auth,
Kubernetes, always-on hosts, backups, security and the threat model, the
serve API, env and config reference, upgrade notes) goes in `docs/operate/`.

A page lives in one section only. Link between sections with relative links
(`../../operate/deploy/index.md`), never absolute docs URLs;
`mkdocs build --strict` fails on any relative link that doesn't resolve.

## Build and preview locally

The build needs Python 3.10 or later (the redirect check needs 3.11).

```bash
python3 -m venv .venv-docs
.venv-docs/bin/pip install -r docs-site/requirements.txt

# live preview (installs the requirements on first run)
bash scripts/serve-docs.sh            # http://127.0.0.1:8000

# what CI and Netlify run
.venv-docs/bin/mkdocs build --strict -f docs-site/mkdocs.yml
.venv-docs/bin/python docs-site/tools/redirects.py check --refs --built
```

Or, without a venv:

```bash
uv run --python 3.12 --with-requirements docs-site/requirements.txt --no-project -- \
  mkdocs build --strict -f docs-site/mkdocs.yml
```

The site builds into `site/` (ignored by git).

## Moving or renaming a page

Old URLs must keep working. After you move a page:

1. Update `tools/page-map.tsv` (old path, new path, sources). Never delete a
   row: the old URL is still out there. Moving a page that is already a
   target means updating every row that points at it, and adding a row for
   its current path. To remove a page, point its rows at the page that
   replaces it.
2. Run `python3 docs-site/tools/redirects.py generate`. It rewrites the
   generated blocks in `mkdocs.yml` (stub pages at the old paths) and
   `netlify.toml` (the 301s).
3. Build, then run `python3 docs-site/tools/redirects.py check --refs --built`.

The check reads the two older sites from git (`--refs`, needs full
history): docs.ligus.ai and docs.langstroth.ai (the two 0.1 sites before
they were merged). Every old page, every path an old host redirected and
every old file must be covered. With `--built` it runs every old URL on both hosts
through an emulation of the Netlify rules and fails on any that doesn't land
on its page in one 301, and on `version.json` being redirected on any host.

## How old URLs are answered

- **docs.langstroth.ai** (the primary domain): every old path of the two
  old sites answers a forced 301 to its new page under `/ligus/` or
  `/operate/`. The old home (`/`) is the new home. Old screenshot URLs
  (`/images/...`) go to `/ligus/images/...`.
- **docs.ligus.ai** (a domain alias of the same site): every old page answers a forced 301 straight to its new page on
  docs.langstroth.ai, and the old home goes to Using Ligus (`/ligus/`).
  Any other path goes to the same path on docs.langstroth.ai, where the
  rules above answer it.
- **version.json** is the exception: on the alias it is served as it is
  (a 200 rewrite listed before the redirects), so update checks in shipped
  desktop builds (`https://docs.ligus.ai/version.json`) never depend on
  following a redirect. Its headers (`Cache-Control: max-age=300`,
  `Access-Control-Allow-Origin: *`) apply on every host. Builds from 0.1 on
  read `https://docs.langstroth.ai/version.json` first and fall back to the
  docs.ligus.ai URL.
- The built site also has a meta-refresh stub page at every old path
  (mkdocs-redirects), so an old link lands on its page even where the
  Netlify rules don't run.

## Netlify setup

One Netlify site, **`langstroth-docs`**, builds and serves everything.
Everything except the domains comes from `netlify.toml`.

| Setting | Value |
|---|---|
| Base directory | `docs-site` |
| Build command | from `netlify.toml` (`pip install -r requirements.txt && mkdocs build --strict`) |
| Publish directory | `docs-site/site` (`site` in the toml) |
| Python | 3.12 (`PYTHON_VERSION` in the toml) |
| Primary domain | `docs.langstroth.ai` |
| Domain alias | `docs.ligus.ai` |

### Owner steps

Do these in order. None of them needs a code change.

1. **Point `langstroth-docs` at the single site.** In the Netlify UI, open
   `langstroth-docs` → Site configuration → Build & deploy → Build settings.
   Set **Base directory** to `docs-site` (it was `docs-site/operator`), and
   leave the build command and publish directory empty so `netlify.toml`
   supplies them. Set the production branch to the branch that carries this
   layout (`release/v0.1.0`, the 0.1.0 release branch, until it merges, then `main`). Deploy, then
   check `https://docs.langstroth.ai/`, `/ligus/`, `/operate/`,
   `/version.json`, and that `/deploy/` answers 301 to `/operate/deploy/`.
2. **Move docs.ligus.ai to `langstroth-docs`.** On `ligus-docs` → Domain
   management, remove `docs.ligus.ai`. On `langstroth-docs` → Domain
   management → Add domain alias, add `docs.ligus.ai`. If docs.ligus.ai's
   DNS record is a CNAME to `ligus-docs.netlify.app`, change it to
   `langstroth-docs.netlify.app` (with Netlify DNS this is done for you).
   Wait for the certificate (HTTPS → Verify DNS configuration; provision it
   if it doesn't start). Then check:
   - `curl -sI https://docs.ligus.ai/version.json` answers **200** (not a
     301) with `access-control-allow-origin: *`;
   - `curl -sI https://docs.ligus.ai/features/git/` answers **301** to
     `https://docs.langstroth.ai/ligus/features/git/`;
   - `curl -sI https://docs.ligus.ai/deploy/` answers **301** to
     `https://docs.langstroth.ai/operate/deploy/`.
3. **Retire `ligus-docs`.** Once step 2 checks out, delete the `ligus-docs`
   site (Site configuration → General → Danger zone → Delete this site), or
   at least stop its builds (Build & deploy → Stop builds).
4. In Domain management, confirm `docs.langstroth.ai` is the primary domain
   and HTTPS is forced (the default once a certificate exists).
