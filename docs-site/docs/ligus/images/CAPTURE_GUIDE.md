# Recapturing the screenshots

The screenshots are captured by scripts, not by hand, from tagged releases
(currently **Ligus v0.3.0** and **lt v0.3.0**), on demo data. Recapturing is
one command per app; the output overwrites the files in this folder with the
names listed in [`README.md`](README.md).

## Desktop

The desktop app's web UI runs in a browser against its fixture backend
(`web/dev/mock`, which never ships), and Playwright drives it:

```bash
git checkout v0.3.0
cd web && bun install && bun run dev:mock      # serves the UI with demo data
node docs-site/tools/screenshots/capture-desktop.mjs   # in another terminal
```

`docs-site/tools/screenshots/README.md` documents the script's options.
It opens each view at 1440 × 900 with a device scale factor of 2, in the
Pollen palette in light and dark (set through the `ligus.theme` and
`ligus.appearance` local-storage keys), crops panes and dialogs, and
compresses the PNGs with `pngquant`.

## lt

lt renders its screens from its demo backend through ratatui's test
backend; the script lives in the ligus-term repository:

```bash
git clone https://github.com/langstroth-ai/ligus-term && cd ligus-term
git checkout v0.3.0
cargo run --example screenshots -- --gallery DIR   # every view, row menus and popups
cargo run --example screenshots -- --themes DIR    # chats and proposals in every palette, light and dark
```

The output is SVG; render it to PNG at 2× (any headless browser), then
compress it.

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
