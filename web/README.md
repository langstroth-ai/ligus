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

### Copy in panels

Panels (the right-rail panes, the sidebar's workspace views, Settings tabs,
the Inbox) open on their content, not on an explanation of it. The rules,
with the primitives in `src/components/ui.tsx`:

- **Pane header = title only.** What the pane is for goes behind the "?"
  beside the title (`RailFrame`'s `about`, which also links the docs page).
  No subtitles.
- **Explanations go behind a "?".** A section that needs one gets
  `<Section info="…">` (an `InfoTip`); a control gets an `InfoTip` beside it.
  The text stays reachable by hover, keyboard focus and screen reader (the
  button is `aria-describedby` a visually hidden copy).
- **No always-shown info boxes.** An explanatory box becomes nothing (if the
  UI already says it), a one-line muted `Hint`, or a `OnceHint` the viewer
  dismisses once (remembered per device in `lib/hints.ts`; blocked storage
  just shows it again, never breaks rendering).
- **Empty states: one short line** (about six words: "No MCP servers
  enabled") and at most one action. Don't explain consequences.
- **Inherited or derived state is a tag, not a banner.** `<Section tag={<MetaTag
  action={…}>from workspace</MetaTag>}>`, with the edit link compact beside it.
- **Short labels.** Section titles are a few words; a card caption is one
  line; don't repeat a word the row already shows ("default runtime ·
  default").
- **Lists are rows, not tiles.** A list in a panel is a `RowList` of
  `ListRow`s: one line (name, muted meta on the same line, a small trailing
  state), rows on the pane background with hairline separators, height from
  the density setting. Details (descriptions, long captions, editors) open on
  the row or sit in its tooltip. A `Card` is for a single object that needs
  attention (a decision, a warning, a form being filled), not for list items.
- **Row actions live in the row menu.** A row's secondary actions (Edit,
  Remove, Revoke, Copy, Rename, Set default, Delete, …) are not buttons around
  the row: they open from a right-click on the row, from the row's `⋯`
  (shown on hover and keyboard focus, always on touch), or with Shift+F10 /
  the Menu key (`ListRow actions`, or `useRowMenu` in `RowMenu.tsx`). The
  row click keeps the row's one primary action (open, select, expand).
  Destructive items go last after a separator and keep their confirm and
  consequence text. A row that needs attention may keep one visible button
  (Approve a pending device, a warning's fix).
- **Never hide what protects someone.** Warnings, blocking states and
  security information stay on screen: sandbox off, lockdown, unverified
  identity, offline hosts, and the consequences shown before an irreversible
  action (remove, revoke, delete, reset).
- **Density and accessibility hold.** Use the shared primitives so the
  density setting applies, and keep every hidden explanation keyboard- and
  screen-reader-reachable through the tooltip layer (`data-tip`).

The full app runs via `cargo tauri dev` from the repo root, which serves this
frontend and the Rust backend together.
