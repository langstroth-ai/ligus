# Screenshot capture

`capture-desktop.mjs` captures the Ligus desktop UI from the web mock
(`web/dev/mock`: demo data, no backend) with Playwright, for the docs,
langstroth.ai and the READMEs. Run it from the root of a checkout of the
release you are documenting:

```bash
git worktree add ../ligus-v0.3.0 v0.3.0 && cd ../ligus-v0.3.0
(cd web && bun install)
node docs-site/tools/screenshots/capture-desktop.mjs --out /tmp/ligus-shots
```

Options: `--out DIR` (default `screenshots-out`), `--only view,view`
(names from `VIEWS` in the script), `--port N` (default 5310). Needs Google
Chrome (or `CHROME=/path/to/chrome`), bun, `cwebp` and `pngquant`
(`brew install webp pngquant`).

It writes `desktop-<name>-<mode>` files, where mode is `light` or `dark`
(Pollen) or `harbor-light` / `harbor-dark`:

| Output | Size | Used by |
|---|---|---|
| `png/desktop-<name>-<mode>.png` | 2x, pngquant | `docs-site/docs/ligus/images/`; langstroth-relay's README (`desktop-settings-sync-*` as `docs/images/ligus-team-sync-*.png`) |
| `png/desktop-<name>-<mode>@1x.png` | 1x, pngquant | `assets/readme/desktop-overview-*.png`; the homebrew-ligus and org profile READMEs (`ligus-desktop-*.png`) |
| `webp/desktop-<name>-<mode>.webp` and `@2x.webp` | 1x and 2x | langstroth.ai `public/shots/` |

langstroth.ai names a few differently: `desktop-hero` is `overview`,
`desktop-review-pane` is `review`, `desktop-tools-pane` is `tools`,
`desktop-inbox` and `desktop-appearance` are the crops.

The demo data is rewritten in the browser as it loads (`REWRITES` in the
script) so no shot shows a real person, account or machine, and one failed
tool call in the hero shows as succeeded. Look at every image before you
commit it.

The current images were captured from Ligus v0.3.0. lt's screenshots come
from `scripts/capture-screenshots.sh` in langstroth-ai/ligus-term.
