# Homebrew cask (staging)

`Casks/ligus-app.rb` is the Homebrew cask for installing the desktop app. It
lives here as a staging copy; Homebrew discovers casks from a **tap repo**,
`github.com/langstroth-ai/homebrew-ligus`, which holds the live copy.

The tap has two entries:

| Name | Kind | Installs |
|---|---|---|
| `ligus` | formula | the `ligus` CLI |
| `ligus-app` | cask | `Ligus.app`, the desktop app |

The cask is `ligus-app` rather than `ligus` because a formula and a cask with the
same name in one tap make `brew install langstroth-ai/ligus/ligus` ambiguous.

Users install with:

```bash
brew install --cask langstroth-ai/ligus/ligus-app   # desktop app
brew install langstroth-ai/ligus/ligus              # CLI
```

`brew upgrade --cask ligus-app` picks up new releases (the `livecheck` block
watches GitHub releases).

## Releases update the tap

`release.yml` bumps the tap after its `publish` job has published the
release, never from a draft: job `cask` sets `version` and `sha256` in
`Casks/ligus-app.rb` from the released `Ligus_<version>_aarch64.dmg`, and job
`formula` rewrites `Formula/ligus.rb` from the released `ligus-<target>`
binaries. Both need `HOMEBREW_TAP_TOKEN` (Contents:write on the tap) and skip
cleanly without it. `update-cask.yml` re-bumps the cask for a past, published
tag by hand.

Never recreate the pre-rename `Formula/hive-cli.rb` or `Casks/hive.rb` in the
tap: its `formula_renames.json` and `cask_renames.json` map those names to
`ligus` and `ligus-app`, so existing installs upgrade in place.

The app is **Apple-Silicon-only**, so the cask is arm-only (`depends_on arch:
:arm64`).

## Signing

The macOS DMGs are **signed + notarized** (Developer ID) in CI, so Gatekeeper
opens them normally. Notarization also satisfies the signing prerequisite for
the official `homebrew/cask` repo; a personal tap is still the pragmatic path
until the project meets homebrew-cask's notability criteria.
