# Brand assets

Two marks, one for the platform and one for the app. Use one per surface; don't
put both side by side.

- **Langstroth** (the platform: the GitHub org, langstroth.ai, the relay, the
  docs) uses the **frame mark**: a hive box seen from above with its movable
  frames.
- **Ligus** (the app: desktop, phone, CLI, `lt`, Homebrew, the App Store) uses
  the **honeycomb mark**. Ligus is short for *ligustica*.

## Langstroth

| File | What it is | Used for |
|---|---|---|
| `langstroth-mark.svg` | The frame mark in the brand colours, transparent | langstroth.ai header and footer (inlined in the site as `FrameMark`), docs |
| `langstroth-mark-mono.svg` | The same mark in `currentColor` | One-colour places: dark/light UI chrome, print |
| `langstroth-org-avatar.svg` | The mark centred on the dark brand tile, square | Source for the org avatar |
| `langstroth-org-avatar-1024.png` | 1024×1024 render of the avatar | The `langstroth-ai` GitHub org profile picture (uploaded by hand: org Settings → Profile) |

Re-render the avatar after changing its SVG:

```sh
rsvg-convert -w 1024 -h 1024 langstroth-org-avatar.svg -o langstroth-org-avatar-1024.png
oxipng -o 4 langstroth-org-avatar-1024.png   # optional, smaller file
```

## Ligus

| File | What it is | Used for |
|---|---|---|
| `ligus-mark.svg` | The honeycomb mark in the brand colours | The app, docs, the Ligus pages of langstroth.ai |
| `ligus-mark-mono.svg` | The honeycomb mark in `currentColor` | One-colour places |
| `ligus-lockup.svg` | Mark plus wordmark | Headers, READMEs |
| `ligus-app-icon.svg`, `ligus-app-icon-1024.png` | The app icon | Desktop and phone app icons (`render-tauri-icons.sh` makes the desktop sizes) |
| `ligus-tray-template.svg`, `ligus-tray-template-128.png` | Monochrome template icon | The macOS menu bar |
| `ligus-brand-tokens.json` | The one colour source: brand colours, the six palettes (light and dark, in OKLCH), status, diff and presence colours, state glyphs, motion | The desktop app, the phone, `lt`, the website (see [Colour tokens](#colour-tokens)) |
| `LigusLogo.html`, `generate-brand-assets.mjs` | Logo reference and the generator | Regenerating the Ligus assets |

The `lt` terminal app's lockups and screenshots live in its own repository
(`langstroth-ai/ligus-term`, `assets/`), built from these marks.

## Colour tokens

`ligus-brand-tokens.json` is the single source for colour on every Ligus
surface. Colours are authored in OKLCH; the generator rounds each one to 8-bit
sRGB, so the desktop, the phone and `lt` paint exactly the same channels.

| Key | What it holds |
|---|---|
| `studio` | The brand colours the logo and icon generator uses |
| `themes` | The six palettes (`pollen`, `studio`, `harbor`, `slate`, `meadow`, `obsidian`), each `light` and `dark`: `canvas`, `ink`, `panel`, `mist`, `line`, `accentWarm`, `accentCool`, the sidebar colours and the `scheme` |
| `schemes` | What depends only on light or dark: the status roles `ok`, `info` (running), `warn`, `danger`, `neutral`; the overlay and on-accent colours; the lightness numbers the desktop derives inks and fills from; diff colours; eight presence colours |
| `states` | One glyph per state (working, needs you, queued, done, failed, paused, warning, no data) for each surface: the desktop icon, the SF Symbol, `lt`'s Unicode glyph and its `--ascii` form |
| `motion` | Standard (140 ms), sheet (220 ms) and state-change pulse (300 ms) durations |

Rules the validator enforces:

- `accentCool` (honey) is a fill, never text; text uses the derived accent ink.
- `accentWarm` means "a person is involved" and is never a status colour; warn
  keeps a measurable distance from every palette's `accentWarm`.
- Status colours always come with a glyph. Even so, every pair of status
  colours stays distinguishable with simulated protanopia, deuteranopia and
  tritanopia.
- Text is at least 4.5:1 and glyphs at least 3:1 on canvas, panel and mist.

Change a colour:

```sh
$EDITOR assets/branding/ligus-brand-tokens.json
node assets/branding/tokens/generate.mjs     # rewrite the generated files
node assets/branding/tokens/validate.mjs     # contrast + colour-vision checks
```

CI runs `generate.mjs --check` (fails if a generated file is stale) and
`validate.mjs`; `web/src/lib/tokens.test.ts` checks that the desktop, the
phone catalogue and the `lt` table carry the same values.

Generated files (never edit them by hand):

| File | Used by |
|---|---|
| `web/src/lib/tokens.generated.ts` | `web/src/lib/theme.ts` (palettes and scheme tokens applied at runtime) |
| `web/src/tokens.generated.css` | `web/src/styles.css` (first-paint defaults, OKLCH with `rgb()` fallbacks) |
| `generated/ios/LigusTokens.xcassets` | The phone app's colour catalogue |
| `generated/ios/LigusTokens.generated.swift` | The same values as `RGBA` literals for the phone's `Theme.swift` |
| `generated/lt/ligus_tokens.rs` | `lt`'s colour table (truecolor, xterm-256 and 16-colour) |

How the phone and `lt` repositories take them is in
[`generated/README.md`](generated/README.md).
