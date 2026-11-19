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

`.github/workflows/rust-tauri.yml` runs `cargo build -p hive-app --release` on the
macOS/Windows/Linux matrix (validates the bundle compiles). Wiring real signing +
attaching artifacts to GitHub Releases is gated on the secrets above and is a release-
engineering step, not part of PR CI.

## Bundle-size note

The frontend bundle is large because Monaco ships every language worker. Before a
production release, trim Monaco to the languages actually used (diff/markdown/json/
plaintext) via the `monaco-editor/editor` entry point plus per-feature and
per-language `register` imports (monaco-editor 0.56+), or a Vite `manualChunks` split.

## Relay

The relay deploys independently of the desktop app and lives in its own repo,
[github.com/langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay)
(`docker build -t langstroth-relay https://github.com/langstroth-ai/langstroth-relay.git`), bind via
`LIGUS_RELAY_ADDR`. See that repo's README + `deploy/fly.toml` for hosting.
