# Themes

Ligus has six palettes, each with a light and a dark variant, in the
desktop app, on the phone and in lt (the Ligus terminal). You can also
bring your own: one theme file, in TOML or JSON, works in the desktop app
and in lt.

## Palettes and appearance

| Palette | Feel |
|---|---|
| **pollen** | honey gold, the default |
| **studio** | neutral graphite |
| **harbor** | ocean blue |
| **meadow** | green |
| **slate** | cool blue-grey |
| **obsidian** | true dark, monochrome (dark in both variants on the desktop) |

**Appearance** picks the variant: **auto** follows the system (the desktop)
or the terminal's background (lt), or choose **light** or **dark**.

- **Desktop:** Settings → Appearance → **Mode** and **Theme**. Changes apply
  at once and are kept on this device.
- **lt:** the **Theme** and **Appearance** rows in Settings, or **Switch the
  theme** in the palette (`ctrl-k`), preview live for the session. To keep
  a choice, set it in lt's `config.toml`:

  ```toml
  theme = "harbor"        # a palette, a custom theme's name, or mono
  appearance = "auto"     # auto | light | dark
  ```

  The older values still work: `theme = "auto"`, `"dark"` and `"light"`
  mean pollen with that appearance; `theme = "mono"` draws without colour.
  `NO_COLOR` gives mono unless a theme is named. lt draws on the
  terminal's own background; on 256- and 16-colour terminals it uses the
  nearest colours (`COLORTERM=truecolor` turns on 24-bit colour;
  `LT_COLOR_DEPTH=truecolor|256|16` overrides the detection).

## Custom themes

A theme file names a palette to start from and the colours it changes, for
a light background, a dark one, or both. Every colour you leave out comes
from the palette in `extends`; a missing `[light]` or `[dark]` table means
that appearance is the palette unchanged.

```toml
# heather.toml
format = "ligus.theme.v1"
name = "Heather"
extends = "meadow"

[light]
canvas = "#fbf7fb"
ink = "#2a1f2b"
accentCool = "oklch(0.48 0.12 330)"
accentWarm = "#a3541c"

[dark]
canvas = "#1b151c"
ink = "#f2e8f2"
accentCool = "oklch(0.78 0.1 330)"
border = "#6a5a6a"
```

The same theme as JSON:

```json
{
  "$schema": "https://docs.langstroth.ai/schemas/ligus-theme.v1.json",
  "format": "ligus.theme.v1",
  "name": "Heather",
  "extends": "meadow",
  "light": {
    "canvas": "#fbf7fb",
    "ink": "#2a1f2b",
    "accentCool": "oklch(0.48 0.12 330)",
    "accentWarm": "#a3541c"
  },
  "dark": {
    "canvas": "#1b151c",
    "ink": "#f2e8f2",
    "accentCool": "oklch(0.78 0.1 330)",
    "border": "#6a5a6a"
  }
}
```

The [JSON Schema](../../schemas/ligus-theme.v1.json) describes the format
for editors that validate as you type.

### Keys

| Key | Meaning |
|---|---|
| `format` | optional; `"ligus.theme.v1"` |
| `name` | the name shown in Settings and the palette, up to 40 characters; defaults to the file name |
| `extends` | `pollen` (the default), `studio`, `harbor`, `meadow`, `slate` or `obsidian` |
| `[light]`, `[dark]` | the colours, at least one of the two |

### Colours

| Token | Colours | App |
|---|---|---|
| `canvas` | the window background; in lt, the background contrast is checked against (lt never paints it) | both |
| `panel` | cards, panes and inputs | desktop |
| `mist` | hover and quiet fills | desktop |
| `line` | hairlines and dividers | desktop |
| `ink` | body text | both |
| `inkMuted` | secondary text | lt |
| `accentCool` | the accent: buttons, links and focus; in lt, headings, focus and the current view | both |
| `accentWarm` | needs you, @mentions | both |
| `onAccent` | text on an accent button | desktop |
| `sidebarTop`, `sidebarBottom` | the sidebar gradient | desktop |
| `sidebarInk`, `sidebarInkMuted` | sidebar text | desktop |
| `ok`, `info`, `warn`, `danger`, `neutral` | done, working, queued or warning, failed, paused | both |
| `diffAddInk`, `diffDelInk` | added and removed lines | both |
| `diffAddBg`, `diffDelBg` | the tint behind added and removed lines | desktop |
| `diffComment` | the review-comment marker | both |
| `border` | pane borders | lt |
| `selection` | the selected row | lt |
| `agent` | agents' names | lt |

Each app uses the tokens it draws with and keeps the rest, so a file made
for one works in the other.

A colour is one of:

- `#rgb`, `#rrggbb` or `#rrggbbaa`
- `rgb(r, g, b)`, `rgba(r, g, b, a)` or `rgb(r g b / a)`: channels 0 to 255,
  alpha 0 to 1
- `oklch(L C H)` or `oklch(L C H / a)`: lightness 0 to 1 (or 0% to 100%),
  chroma 0 to 0.5, hue 0 to 360

Nothing else is accepted: no colour names, `var()`, `calc()` or `url()`.
A value is at most 64 characters and a file at most 64 KB.

### Checks

A bad colour, a key with the wrong type or an unknown `extends` refuses the
file, with the line it's on. An unknown key is a warning and is ignored.
Colours that are hard to read are warnings too: text below 4.5:1 against
its background, or status colours and accents below 3:1. The theme is
kept; fix the colours when you like.

### On the desktop

Settings → Appearance:

- **Import a theme…** reads a `.toml` or `.json` file. The theme is added to
  your themes and used straight away; any warnings are listed under the
  buttons.
- **Export current theme** saves the palette or custom theme in use as a
  full file, every colour for light and dark, to start your own from.
- A theme's row menu (right-click, or `⋯`) has **Rename** and **Remove**.

Imported themes are kept on this device, up to 20. A phone set to match
the desktop gets the palette the theme extends.

### In lt

Put theme files in a `themes` folder next to lt's `config.toml`
(`lt --print-config` and Settings show where it is), as
`themes/<name>.toml` or `themes/<name>.json`, then pick it by name:
`theme = "heather"`. Custom themes appear in Settings and in **Switch the
theme**.

```sh
lt themes list            # the palettes and your themes
lt themes check heather.toml   # errors with line numbers, warnings, contrast
```

Both take `--json`.
