# Generated colour files for the phone and `lt`

Everything in this directory (except this README) is written by
`node assets/branding/tokens/generate.mjs` from `../ligus-brand-tokens.json`.
Don't edit these files; change the JSON and rerun the generator. CI fails if
they are stale.

The phone app (`langstroth-ai/ligus-ios`) and `lt` (`langstroth-ai/ligus-term`)
live in their own repositories. They copy these files in and keep a parity test
against them, so a colour changes in one place and every surface follows.

## Phone (`ios/`)

- `LigusTokens.xcassets`: a colour catalogue. Groups are namespaced, so a
  colour is named `pollen/canvas`, `status/warn`, `diff/addBg` or
  `presence/presence3`. Each colour has a light value and a dark-appearance
  value (Obsidian's two are the same). Copy the folder into the app target
  and use `Color("status/warn")`.
- `LigusTokens.generated.swift`: the same values as literals for the types in
  `Ligus/App/Theme.swift` (`RGBA`, `Palette`, `PaletteName`):
  `LigusTokens.families[.pollen]`, `LigusTokens.light.warn`,
  `LigusTokens.states["queued"]?.sfSymbol`. Copy it next to `Theme.swift`.
  The app's OKLCH ink derivations and contrast tests work on these values
  unchanged.

To update the phone: copy both, then run its tests. A parity test there
should assert that every `PaletteName` family equals
`LigusTokens.families[name]` and that the status colours equal
`LigusTokens.light` / `LigusTokens.dark`.

## `lt` (`lt/`)

- `ligus_tokens.rs`: `DARK` and `LIGHT` (`SchemeTokens`) and `STATE_GLYPHS`.
  Each colour is a `TokenColor` with three depths:
  - `rgb`: truecolor (`COLORTERM=truecolor` or `24bit`);
  - `xterm256`: the nearest 256-colour index, 16..255 (never the user's own
    0..15);
  - `ansi16`: the 16-colour slot (ok green, danger red, warn yellow, info
    cyan, neutral bright black). `None` means no hue at 16 colours: draw the
    accent and focus with bold or reverse, because yellow belongs to warn.

  Status colours are the text-weight inks, legible on the terminal's own
  background. `STATE_GLYPHS` has each state's glyph and its `--ascii` form.

To update `lt`: copy the file to `src/theme_tokens.rs`, build `Theme::DARK`
and `Theme::LIGHT` from it, and keep a test that the two agree.
