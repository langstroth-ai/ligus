# Documentation sites

The published docs are two MkDocs (Material) sites built from this directory.

| Site | Directory | Domain | Audience |
|---|---|---|---|
| Ligus user docs | `user/` | <https://docs.ligus.ai/> | People using the Ligus desktop app, phone app and `ligus` CLI |
| Langstroth operator docs | `operator/` | <https://docs.langstroth.ai/> | People self-hosting and deploying: relay, ligus-auth, Helm charts, `ligus serve` and worker hosts, security model, operator reference |

*Ligus* is short for *ligustica*, the Italian honeybee; Langstroth is the
platform behind the apps. Developer and maintainer docs are in
[`../docs/`](../docs/README.md), not here.

```
docs-site/
  mkdocs-base.yml     settings both sites share (theme, Markdown extensions)
  requirements.txt    pinned build dependencies for both sites
  theme/              shared logo, favicon and stylesheet (theme custom_dir)
  user/               docs.ligus.ai: mkdocs.yml, netlify.toml, docs/
  operator/           docs.langstroth.ai: mkdocs.yml, netlify.toml, docs/
  tools/page-map.tsv  where every page of the old single site went
  tools/redirects.py  generates and checks the redirects
```

## Which site a page belongs on

Using the apps (workspaces, agents and runtimes, people and DMs, workflows,
proposals, passkeys as users see them, the CLI basics, what's new) goes in
`user/`. Running the platform (deployment shapes, the relay and auth,
Kubernetes, always-on hosts, backups, security and the threat model, the
serve API, env and config reference, upgrade notes) goes in `operator/`.

A page lives on one site only. When the other audience needs it, give them a
short summary and an absolute link (`https://docs.ligus.ai/...` or
`https://docs.langstroth.ai/...`). Relative links must stay inside their own
site; `mkdocs build --strict` fails on any that don't resolve.

## Build and preview locally

```bash
python3 -m venv .venv-docs
.venv-docs/bin/pip install -r docs-site/requirements.txt

# live preview (installs the requirements on first run)
bash scripts/serve-docs.sh user        # http://127.0.0.1:8000
bash scripts/serve-docs.sh operator 8001

# what CI and Netlify run
.venv-docs/bin/mkdocs build --strict -f docs-site/user/mkdocs.yml
.venv-docs/bin/mkdocs build --strict -f docs-site/operator/mkdocs.yml
.venv-docs/bin/python docs-site/tools/redirects.py check --built
```

Each site builds into its own `site/` directory (ignored by git).

## Moving or renaming a page

Old URLs must keep working. After you move a page:

1. Update `tools/page-map.tsv` (old path, site, new path). Never delete a
   row: the old path is still out there.
2. Run `python3 docs-site/tools/redirects.py generate`. It rewrites the
   generated blocks in `user/mkdocs.yml` (stub pages for GitHub Pages),
   `user/netlify.toml` and `operator/netlify.toml`.
3. Run `python3 docs-site/tools/redirects.py check --built` after building
   both sites.

## Netlify settings

Create two Netlify sites from this repository. Everything else comes from the
`netlify.toml` in each base directory.

| | User docs | Operator docs |
|---|---|---|
| Base directory | `docs-site/user` | `docs-site/operator` |
| Build command | from `netlify.toml` | from `netlify.toml` |
| Publish directory | `docs-site/user/site` (`site` in the toml) | `docs-site/operator/site` (`site` in the toml) |
| Python | 3.12 (`PYTHON_VERSION` in the toml) | 3.12 |
| Primary domain | `docs.ligus.ai` | `docs.langstroth.ai` |
| Domain aliases | `docs.apiaryhq.ai` (the 1.x docs domain) | none |

Each toml sets the build command
(`pip install -r ../requirements.txt && mkdocs build --strict`), the publish
directory, security headers (HSTS, CSP, frame and referrer policy), an
`ignore` command that skips a deploy when nothing the site is built from
changed, and the 301 redirects.

- **docs.ligus.ai** answers every page that moved to the operator site, and
  the two pages that moved to `help/`, with a 301 to the new address.
- **docs.apiaryhq.ai** must be attached to the user site as a domain alias.
  Host-specific rules in `user/netlify.toml` send every page of the old site
  straight to its new home on either site, and any other path to the same
  path on docs.ligus.ai, so no old link 404s.
- **docs.langstroth.ai** sends user-site paths asked for on it to
  docs.ligus.ai.

The desktop app's update check reads `https://docs.ligus.ai/version.json`,
so that file stays on the user site.

## GitHub Pages (until DNS moves)

`.github/workflows/docs.yml` still publishes the user site to GitHub Pages
(with `user/docs/CNAME`). While Pages serves docs.ligus.ai, moved pages are
answered by the meta-refresh stubs that mkdocs-redirects writes. Once
docs.ligus.ai and docs.apiaryhq.ai point at Netlify, delete the workflow,
turn off Pages for the repository and remove `user/docs/CNAME`.
