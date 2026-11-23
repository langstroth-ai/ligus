# `web` — Ligus frontend

The **React 19 + TypeScript** frontend, built with **Vite**. It runs inside the
[Tauri desktop shell](../app/README.md) (not a standalone web server) and talks
to the Rust backend over IPC.

## Layout

- `src/components/` — the UI (chat, diff view, right-rail Review/Workflows panes,
  onboarding, settings, workflow builder, …).
- `src/lib/ipc.ts` — the **Tauri IPC** layer: invokes backend commands and
  subscribes to emitted events. Other `src/lib/` modules cover mentions, slash
  commands, templates, workflow helpers, theme, and attachments.
- `src/bindings/` — **generated** TypeScript types, mirrored from the Rust DTOs
  in [`ligus-proto`](../crates/ligus-proto/README.md). Do not hand-edit; regenerate
  with `cargo test -p ligus-proto export_bindings`.
- Data fetching uses **TanStack Query**; routing uses TanStack Router.

## Dev commands

```bash
bun install        # or: npm install
bun run dev        # Vite dev server (usually driven via `cargo tauri dev`)
bun run dev:mock   # browser preview on :5199 with a canned backend (dev/mock)
bun run build      # tsc --noEmit typecheck + vite build
bun run test       # vitest
bun run typecheck  # tsc --noEmit
```

> **Node.js 22 or newer is a prerequisite.** `bun run test`, `bun run build` and
> `bun run dev` launch Vitest, Vite and tsc through their Node shebangs, and
> Vitest 5, jsdom 31 and jest-dom 7 require Node 22+ (jsdom asks for 22.22.2+,
> 24.15+ or 26+). `esbuild` stays listed in `trustedDependencies` so its
> postinstall can run if a dependency pulls it in again, but Vite 8 bundles with
> Rolldown and the current lockfile does not install it.

`dev:mock` swaps the Tauri IPC modules for `dev/mock/` (see
`vite.mock.config.ts`) so the UI renders in a plain browser with fixture data —
handy for design work and screenshots. Query knobs: `?scenario=onboarding|empty`,
`&dearth=1`, `&lockdown=1`, `&loading=1`, `&error=1`. The production build never
references `dev/`.

The shared presentational components (`src/components/kit/`: session status
and card, sheet, stepper, data table, combobox, fingerprint, presence and
avatar stack, inbox item, status bar shell, countdown, virtual list) have a
gallery page in mock mode at `/dev/gallery/` (`?theme=harbor&mode=dark`,
`&sheet=1` to open the sheet).

### Design QA

`bun run design-qa` (with `bun run dev:mock` running) renders the mock app in
headless Chrome across seven themes and six views and checks each capture:
text contrast (WCAG AA), no text below 11px, at most one banner (elements
marked `data-calm="banner"`), at most three count badges and at most four
accent-filled regions. What already breaks a rule is recorded in
`tools/design-qa-baseline.json` and tolerated; anything new fails. After a
change that fixes some of it, shrink the baseline with
`node tools/scan.mjs --update-baseline` (it never grows). `--out DIR` keeps
the screenshots and a `report.json`; `--only chat,review` limits the views.
CI runs it in `.github/workflows/design-qa.yml` and uploads the screenshots.

The full app runs via `cargo tauri dev` from the repo root, which serves this
frontend and the Rust backend together.
