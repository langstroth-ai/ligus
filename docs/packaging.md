# Packaging & release (Rust + Tauri)

Cross-platform bundles are produced by the Tauri bundler. `app/tauri.conf.json`
has `bundle.active = true` and `targets = "all"`, so a release build emits the native
installer for the host OS.

## Build

```bash
cd web && bun install && cd ..  # once
cargo tauri build               # from repo root: builds web/ (Bun hook) then bundles
```

Per-OS artifacts:

| OS      | Artifacts                          | Signing |
|---------|------------------------------------|---------|
| macOS   | `.app`, `.dmg`                     | Developer ID + **notarization** |
| Windows | `.msi`, `.exe` (NSIS)              | **Authenticode** code signing |
| Linux   | `.deb`, `.AppImage`, `.rpm`        | (optional) GPG / repo signing |

The app's name everywhere users see it (window title, `Ligus.app`, the Start
menu and desktop shortcuts, the Linux menu entry, installer file names) is
`productName`, **Ligus**. The executable inside is `ligus-desktop`
(`mainBinaryName`): `Ligus.app/Contents/MacOS/ligus-desktop`,
`ligus-desktop.exe` in the Windows install dir, `/usr/bin/ligus-desktop` from
the `.deb`/`.rpm` (the generated `.desktop` file's `Exec=` points at it). It
must not be `Ligus`: on macOS's case-insensitive filesystem that is the same
file as the CLI's `target/release/ligus`, and building the app overwrote the
CLI. `scripts/check-release-config.sh` and `app/tests/binary_names.rs` fail a
name that collides with a workspace binary ignoring case. Keychain trust
doesn't depend on the executable name: a release app is trusted by its
designated requirement (bundle id `ai.ligus.desktop` and team id).

## Signing (requires real credentials — not run in CI by default)

- **macOS** — set `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`; Tauri signs
  and notarizes during `tauri build`.
  - **Native passkeys** need the associated-domains entitlement
    (`app/Ligus.entitlements`: `webcredentials:ligus.ai`, app id
    `H5H3NNR456.ai.ligus.desktop`) and, because that entitlement is restricted, a
    Developer ID provisioning profile with Associated Domains embedded as
    `Contents/embedded.provisionprofile`. Put the profile at
    `app/embedded.provisionprofile` (gitignored) and build with
    `cargo tauri build --config app/tauri.passkeys.conf.json`. Never sign the
    entitlement without the profile: macOS refuses to launch the app. The site must
    serve `https://ligus.ai/.well-known/apple-app-site-association` with
    `{"webcredentials":{"apps":["H5H3NNR456.ai.ligus.desktop", ...]}}`. See
    docs-site `operate/security/native-passkeys.md`.
- **Windows** — provide a code-signing cert; configure `bundle.windows.certificateThumbprint`
  (or sign the produced `.msi`/`.exe` via `signtool`).
- **Linux** — unsigned AppImage/deb are usable; sign repos/packages as needed.

Tauri's built-in **updater** can be enabled later with an update signing keypair
(`tauri signer generate`) and a release feed.

## CI

`ci.yml` (required check `ci-ok`, also in the merge queue) builds and tests the
workspace on Linux, macOS and Windows, which are the desktop app's release
triples. Its `cli-targets` job builds the `ligus` CLI for every target
`release-cli.yml` ships (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`)
through the same composite action, `.github/actions/build-cli`, so a target that
doesn't build fails the PR, not the tag. `cut-release.yml` won't tag a commit
unless `ci-ok` passed on it. `ci-nightly.yml` builds the release
profile. `release-lint.yml` runs `scripts/test-release-workflow.rb`, which checks
the structure below.

## How to cut a release

Cut releases from GitHub with **`cut-release.yml`**, never by tagging on a laptop.

1. Bump every version (the app in `app/tauri.conf.json`, every `ligus-*` crate,
   `web/package.json`) in a PR and merge it through the queue.
2. A repository admin runs:

   ```bash
   gh workflow run cut-release.yml --repo langstroth-ai/ligus -f version=X.Y.Z
   # or pick the commit: -f ref=<branch or sha>
   ```

   By default `ref` is the head of `release/vX.Y.Z` if that branch exists, or `main`
   if it doesn't. The workflow refuses to run unless whoever started it (or re-ran it)
   is a repo admin, the required check `ci-ok` passed on that exact commit (GitHub
   Actions' own check run), `scripts/check-release-config.sh vX.Y.Z` passes there,
   and the tag doesn't exist yet. Then it pushes the annotated tag `vX.Y.Z` on that
   commit and calls `release.yml` with the tag.
3. Watch the run. When it finishes, the release is a published **prerelease** and the
   tap has been bumped. Write the notes and promote it by hand:

   ```bash
   gh release edit vX.Y.Z --title "Ligus vX.Y.Z" --notes-file notes.md --prerelease=false --latest
   ```

**Only one pipeline run per tag.** `cut-release.yml` pushes the tag with
`GITHUB_TOKEN`, and GitHub doesn't start workflows for events that `GITHUB_TOKEN`
causes. So `release.yml`'s own `push: tags` trigger doesn't fire for that tag, and the
`workflow_call` from `cut-release.yml` is the only run. That trigger stays as a
fallback: an admin who pushes a `v*` tag by hand starts the same pipeline. Never give
`cut-release.yml`'s checkout a PAT or an App token: the tag push would then start a
second run. (`release.yml`'s per-tag concurrency group would queue that second run,
and it would only re-fill the draft or stop at the published release, but it is still
wasted work.)

### What the pipeline does

`release.yml` runs the whole release in one workflow run. Its entry points are a call
from `cut-release.yml`, a tag push, or `workflow_dispatch` for an existing tag:

1. `release`: checks the tag against every version (`scripts/check-release-config.sh`)
   and creates the tag's GitHub Release as a **draft** prerelease
   (`scripts/ensure-release.sh`). A rerun reuses the draft. A release that is
   already published is refused.
2. `build` (desktop, macOS/Windows/Linux, tauri-action by release id) and `cli`
   (`release-cli.yml`, four targets, `gh release upload`) upload into the draft.
   Nobody can see a draft: not users, not `install.sh`, not the updater.
3. `verify-updater` checks `latest.json` on the draft (`scripts/verify-latest-json.py`).
4. `publish` runs only if every job above succeeded. It checks the draft holds every
   asset (`scripts/check-release-assets.py`: each CLI binary with a matching
   `.sha256`, the DMG, finished uploads) and a `latest.json` for all three
   platforms, then publishes the draft as a **prerelease**. It never marks it Latest.
5. `cask` and `formula` bump the Homebrew tap, only after `publish`.

### When a release run fails

Nothing is published: the release is still a draft, and the tap is untouched.

- **A flaky job** (runner, network): use "Re-run failed jobs" on the run. Uploads
  replace the target's files in the same draft, and `publish` and the tap jobs run
  once everything is green.
- **A real bug**: fix it through the merge queue, then delete the draft and the tag
  and cut again:

  ```bash
  gh release delete vX.Y.Z --repo langstroth-ai/ligus --yes   # the draft
  git push origin :refs/tags/vX.Y.Z
  gh workflow run cut-release.yml --repo langstroth-ai/ligus -f version=X.Y.Z
  ```

  `gh release delete` finds drafts by tag. Leave out `--cleanup-tag` and delete the
  tag yourself, as above, so you can see that it has gone. `cut-release.yml` refuses
  a tag that still exists.
- **A run that published, then a tap job failed**: rerun the failed job, or bump the
  cask by hand with `update-cask.yml` (it refuses a tag that is still a draft).

A manual run (`workflow_dispatch` with a tag) works on the tag's existing draft. With
a `platforms` subset it fills in those platforms, and publishes only if the draft is
then complete.

Re-cutting a tag whose release was **already published** means deleting that
release first (`ensure-release.sh` refuses to build into a published release),
then deleting the tag and cutting again as above. Users who already downloaded the
old assets keep them, and the tap may point at the old checksums until the new
run's `cask` and `formula` jobs run.

## Bundle-size note

The frontend bundle is large because Monaco ships every language worker. Before a
production release, trim Monaco to the languages actually used (diff/markdown/json/
plaintext) via the `monaco-editor/editor` entry point plus per-feature and
per-language `register` imports (monaco-editor 0.56+), or a Vite `manualChunks` split.

## Relay

The relay deploys independently of the desktop app and lives in its own repo,
[github.com/langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay)
(`docker build -t langstroth-relay https://github.com/langstroth-ai/langstroth-relay.git`), bind via
`LANGSTROTH_RELAY_ADDR`. See that repo's README + `deploy/fly.toml` for hosting.
