# Recapturing the screenshots

The screenshots are captured by scripts, not by hand, from tagged releases
(currently **Ligus v0.3.0** and **lt v0.3.0**), on demo data. Recapturing is
one command per app; the output overwrites the files in this folder with the
names listed in [`README.md`](README.md).

## Desktop

The desktop app's web UI runs in a browser against its fixture backend
(`web/dev/mock`, which never ships), and Playwright drives it. The script
starts the mock server itself:

```bash
git worktree add ../ligus-v0.3.0 v0.3.0 && cd ../ligus-v0.3.0
(cd web && bun install)
node docs-site/tools/screenshots/capture-desktop.mjs --out /tmp/ligus-shots
cp /tmp/ligus-shots/png/desktop-*-light.png /tmp/ligus-shots/png/desktop-*-dark.png docs-site/docs/ligus/images/
```

`docs-site/tools/screenshots/README.md` lists what it writes and which
file goes where (these docs, langstroth.ai, the READMEs). Each view is
opened at 1440 × 900, at scale 1 and 2, in the Pollen palette light and
dark (plus Harbor for the themes shots); panes and dialogs are cropped,
PNGs compressed with `pngquant` and WebP made with `cwebp`. Copy only the
files a page links.

## lt

lt renders its screens from its demo backend through ratatui's test
backend; the script lives in the ligus-term repository:

```bash
git clone https://github.com/langstroth-ai/ligus-term && cd ligus-term
git checkout v0.3.0
scripts/capture-screenshots.sh /tmp/lt-shots   # every view, Pollen light and dark: SVG, PNG, WebP
cp /tmp/lt-shots/png/lt-chats-light.png /tmp/lt-shots/png/lt-chats-dark.png docs-site/docs/ligus/images/
```

## Phone

The iPhone app's demo workspace runs in the simulator with no network
(`-mockServer -demo`; see `docs/DEVELOPMENT.md` in the ligus-ios
repository):

```bash
xcrun simctl launch booted <bundle id> -mockServer -demo -tab chats
xcrun simctl launch booted <bundle id> -mockServer -demo -openProposal p1
xcrun simctl io booted screenshot phone-proposal-dark.png
```

Switch light and dark with `xcrun simctl ui booted appearance light|dark`.

## Before you commit

- Look at every image. Check there is no real name, email, hostname, room id,
  token or invite code in it: only the demo data.
- Keep the file names; pages link them.
- `mkdocs build --strict -f docs-site/mkdocs.yml` must pass.
