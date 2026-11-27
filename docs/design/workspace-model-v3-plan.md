# Workspace model v3: the 0.5 plan

Status: plan for Ligus 0.5, from [`workspace-model-v3.md`](workspace-model-v3.md)
(the design, with the owner decisions in its section 19). Section numbers
below (5.3, 17.4, ...) and test ids (L1, H3, U2, ...) refer to that note.

**In short:** three streams run in parallel. **A** builds rules version 4
and everything an agent needs to say what it runs, where, and what happened,
plus the lifecycles. **B** makes the box act for the person behind each
phone and gives box operators real commands. **C** builds the onboarding
surfaces on top: the first-run fork, linking with a short code, passkey
hand-off from terminals, the one-step agent, the checklist with Clean up, and
the phone in one flow. Two B steps are already under way.

Sizes: **S** at most a day, **M** two to four days, **L** about a week or
more. Repos: `ligus` (core, runtime, cli, app, web, docs), `lt` (ligus-term),
`ligus-ios`, `langstroth-relay`.

## 1. Gates for every step

- Each step lands as one PR (or one PR per repo when it spans repos), with
  its tests from the design note written first and shown failing on `main`.
- Every step that changes authz, the wire, the tunnel, the mailbox or
  storage gets a security pass that proves each finding with a test before
  fixing it, and code-verified docs (help text, the docs site, What's new) in
  the same PR.
- No wire id is renamed; new kinds and fields are additive; no model key goes
  on the log; the UI stays calm (one primary action per pane, details behind
  (i)).
- Anything that changes what the fold allows goes behind rules 4 (A7). Before
  A7 merges, every A and C step must be correct on rules 3.

## 2. Stream A: rules 4 core, agent run, model and placement, lifecycles

| Id | Step | Repos | Size | Depends on | Gating tests |
|---|---|---|---|---|---|
| A1 | **Order and clocks.** Every "latest", "newest" and "after" query by `(lamport, event_id)` (`latest_contact_of`, `messages_after`, snapshot cadence, `list_in`, `list_after`); the `row_id` allow-list lint; turn takeover age by envelope time (7.1, 7.3) | ligus core, runtime | S | — | L2, L4 |
| A2 | **`user_version` 2 and log hygiene.** M8 tables; `pruned_events` tombstones checked at ingest; `dispatch_processed` with the 24-hour envelope-time bound; M10 backfill and seed; prune at most daily and off the launch path; chunk coalescing (2 s or 4 KiB); stop non-seed snapshots (7.2 to 7.4) | ligus runtime, app | M | A1 | L3, L5, E3, V1, V2 |
| A3 | **Typed views.** `WorkspaceConfig` and `ChatState` projections; the guard overlay as a function every reader uses; `WorkspaceId`/`RoomName` and `TurnId` newtypes; the `Turn` view; writers stop authoring whole-list and chat-level agent events (8.2, 11) | ligus core, runtime | M | — | W4, E6 |
| A4 | **Host offers.** Sticky `offers` on host records (still filling `model_gateways`); `host.toml`; the beat carries readiness, model lists, build version and refusals; desktops, lt and the CLI publish with "Don't list this computer's tools"; readers and pickers ignore host records not signed by their device; `last_seen` only on change; one online window; secrets resolve only when `host.toml` declares them (5.1, 5.5, 5.10) | ligus core, runtime, cli (worker, serve), app, web; lt | M | — (reads B1's default gateways file when present) | H6, H7, H11 |
| A5 | **Agent run and placement fields.** Sticky `run` and `placement` with legacy-wins and the merged record; `legacy_id`; the fold-only `host_gone`; M13 on rules 3 (mirror `runtime:<id>`, never rewrite `runtime_id`); the client refuses to place an agent on a host that doesn't offer its source (5.2, 5.3, 5.6) | ligus core, runtime, app | L | A3 | L1, H8, H14, H15 |
| A6 | **The dispatch outcome.** `TurnRoute`; `Queued` and `Unrunnable` rows in the chat on the desktop, lt and the phone; no silent substitution ("Use Claude Code instead" per turn); remove the offline answer-locally fallback; "Run here this time" (rules 3: the owner's own devices); desktop host policy (own person's agents unless opted in); a worker refuses a full run spec it can't resolve and keeps `*` for legacy ids; `MessageCompleted.outcome`, an overridable `canceled`, display time and transcript order (5.4, 8.1) | ligus core, runtime, app, web, cli (worker, serve); lt; ligus-ios (render only) | L | A2, A4, A5 | H1, H2, H3, H9, H13, H16 (worker), E1, E2, E7 |
| A7 | **Rules 4.** `RULES_SUPPORTED = 4`; G1 to G14 in the fold, judged on the merged record; `TurnDelegated`; the default agent's reserved id and optional record; new workspaces found on rules 4; a `compat/v04` crate and the runtime mixed-version test against pinned 0.4 builds (15, 16.3) | ligus core, runtime | L | A4, A5, A6, A8 (the `MemberLeft` kind) | L6, L7, L8, W1, W2, H5, H10, H12, E4, H16 (pinned 0.4) |
| A8 | **Member exits and device lifecycles.** `MemberLeft` (display "asked to leave" on rules 3; the G4 rule lands in A7); My devices and People show Pending, Active, Idle and Revoked with Remove; lt follows synced renames of itself; the certifying desktop writes the phone's `DeviceLabelSet` at enrolment (10.1, 10.4) | ligus core, runtime, app, web; lt | M | A3 | W3, U5 |
| A9 | **Rules-4 switch and notices.** Settings > Security switch with the readiness list (builds below 0.5 from beats, unresolved placements, unsigned host records, accountless members); M13's rules-4 "Pin @x to …" offers; the "old copy of @x in this chat" notice; the "address visible to every member, move it to the host" notice with its one action (5.5, 11, 15.2) | ligus app, web, runtime | M | A4, A7 | L7 (UI), H15 |
| A10 | **Local tables.** `scratch_repos` and `scratch_repo_chats` with the import (M9); `workspace_folders` and one `workspaces.json` (v2 10.1 and 10.2); lt's read marks keyed by workspace (9, 13) | ligus runtime, app, cli; lt | M | A2 (M8) | E5, V1, V3 |

Critical path: A3, A5, A6, A7, A9. A1, A4, A8 and A10 run beside it.

## 3. Stream B: principals on the box, and box commands

B1 and B2 were started separately before this plan; the plan only records
how the rest depends on them.

| Id | Step | Repos | Size | Depends on | Gating tests |
|---|---|---|---|---|---|
| B1 | **Box and gateway commands (in progress).** `ligus gateway add/list/check/remove` writing the default gateways file in the data dir (the env var still wins), the exclusive-mode line; `ligus-local upgrade` (upgrade, restart, verify, report version) with hive-era cleanup (`cleanup --legacy`: units, LaunchAgents, old data dirs, shown before removal); the formula's `service` block (17.7) | ligus cli, deploy; homebrew tap | M | — | gateway command tests; `deploy/tests` for upgrade and cleanup |
| B2 | **serve principals (in progress).** `rt/principal.rs`; every request through the box acts for the phone's person: unread, agent availability, transcript "mine", read marks (`serve-state.json` v2, M11), question cap per person and hour; direct-path per-phone authentication (the shared bearer alone is `Unresolved`); phone actions phone-signed or refused; unresolved phones fail closed (no previews, no "you" notices) (6) | ligus runtime, cli (serve); ligus-ios (direct-path signing) | L | — | U1, U2, U3, U8 |
| B3 | **Secrets on a host.** `ligus secrets set NAME -`, `list` (names only), `rm`; per-workspace secret declarations in `host.toml` feed `provider` offers (5.1, 17.7) | ligus cli, runtime | S | A4 (the `host.toml` format) | H7 (declared-only), V3 |
| B4 | **Pairing lifecycle, both ways.** `phones.json` v2 (`deviceId` required for new pairings, revoked records purged after 30 days, `ownerPhone` dropped); `DELETE /v1/tunnel/pairing`; the desktop Inbox row "unpaired from box-1, remove it?"; `previousDeviceId` at key reset with the revoke offer; the phone's Forget calls the box first and sends preferences to every paired box (10.2, 10.3) | ligus cli (serve), runtime, app, web; ligus-ios | M | B2 | U4, U7 |
| B5 | **The box as a host.** Writes its label only when `--label` changed (M12); `touch_host` moves to the beat; publishes offers and its build in the beat; `ligus serve leave` (10.1, 10.4, 5.10) | ligus cli (serve) | S | A4, A8 | U6, W3 (box leave) |
| B6 | **Admit a box by code match.** `ligus setup box --to <approval address>` and `ligus-local configure --admit-to`; nothing pasted (17.7) | ligus cli, runtime, deploy | S | — | admission tests with code match |
| B7 | **"Update available" for boxes.** The desktop host row and the phone's Health show the box's build against the latest release, with the `ligus-local upgrade` line (17.7) | ligus app, web; ligus-ios | S | A4 (build in beat), B1 | app and phone tests |

## 4. Stream C: onboarding surfaces

| Id | Step | Repos | Size | Depends on | Gating tests |
|---|---|---|---|---|---|
| C1 | **First-run fork.** "Is this your first device with Ligus?" before a name, on the desktop and lt; Secure (passkey and recovery key) inside the first run; the first workspace made for the person; theme out of the first run; the invite guard hint (17.1) | ligus web, app; lt | M | — (the "another device" branch uses the paste path until C2 and C3) | S1 |
| C2 | **The desktop links itself.** Expose the runtime's link request, code-match requester and grant redeem in the app; defer the identity bootstrap until the link completes (17.2) | ligus app, web, runtime | L | — | S1, the code-match approval tests |
| C3 | **Short codes.** The additive relay route; Add a computer shows a 6-character code; `ligus devices link --code` (exit 4 waiting, 0 linked, 7 unreachable); lt's entry; requesters post again after a relay restart and say so (14, 17.2) | langstroth-relay; ligus runtime, cli, app, web; lt | M | C2 (desktop requester), relay route first | S2 |
| C4a | **Passkey hand-off: core, CLI and desktop.** The `Presence` mailbox subject; the CLI as requester (exit 4, `--wait`, `next:`); the desktop as approver (Inbox row and notification, summary from the draft, Not me row); rate limits (17.3) | ligus runtime, cli, app, web | L | — | S3 |
| C4b | **Passkey hand-off: phone and lt.** The phone approves (push to the person, then the same sheet); lt as requester with a spinner (17.3) | ligus-ios; lt; ligus cli (serve) | M | C4a, B2 (push to the phone's person) | S3 (phone), lt tests |
| C5 | **The one-step agent.** One engine form: sources from every placeable host's offers with beat readiness and model lists; "Runs on" default with its one line; test turn; rules 3 reuses runtimes with "Confirm 1 of 2"; rules 4 writes one event; the old side paths (first-run agent step, Add runtime as the main path, the Tools-pane dialog) removed; lt's form and `ligus setup agent` use the same form and the hand-off (17.4) | ligus runtime (setup), app, web, cli; lt | L | A4, A5, C4a (terminal passkey) | H4 |
| C6 | **Change model and Runs on.** The agent's ⋯ menu, through the same form; on rules 4 a model change in one source needs no passkey (5.7, 17.4) | ligus web, runtime (setup) | M | C5, A6 | H4 (rebinds), H10 on rules 4 |
| C7 | **"Set up Ligus" and Clean up.** Checklist rows from setup status plus facts on the desktop (Settings > Set up, empty Home, command palette), `ligus setup`, lt and the phone's Health; Clean up for hosts of former members, never-finished devices, retired or "asked to leave" members and unpaired phones, one passkey each with a step count (17.5) | ligus runtime (setup), app, web, cli; lt; ligus-ios | L | A8, B4 (for every row; it can start with today's facts) | S4 |
| C8 | **The phone in one flow.** Consent signed inside the request; name prefilled; label written by the certifying desktop; push asked in the flow, grey "Not available in this build" otherwise; the phone renames itself (17.6) | ligus-ios; ligus app | M | A8 (label at enrolment) | U5, phone onboarding tests |
| C9 | **Docs.** One "Set up Ligus" guide per surface (desktop, lt, CLI, phone, box), an lt getting-started page, the box and gateway pages, What's new; checked against the code | ligus docs-site | M | runs beside every step; finishes last | `mkdocs build --strict` |

## 5. How the streams run in parallel

Three people (or agents) can take one stream each. The waves below are the
order inside each stream; a step starts when its dependencies have merged.

| Wave | Stream A | Stream B | Stream C |
|---|---|---|---|
| 1 | A1, A3, A4 | B1, B2 (in progress), B6 | C1, C2, C4a |
| 2 | A2, A5, A8 | B3, B5 | C3, C8 |
| 3 | A6, A10 | B4, B7 | C5, C4b |
| 4 | A7 | (security pass on B) | C6, C7 |
| 5 | A9 | — | C9 |

Cross-stream dependencies, all on merged steps:

- C5 needs A4 (offers) and A5 (run and placement), and C4a for terminals.
- C4b needs B2, so a hand-off push reaches the right person's phone.
- C7 needs A8 and B4 for its full set of rows.
- B3, B5 and B7 need A4's `host.toml` and beat format.
- A4 reads B1's default gateways file; until B1 merges it reads the env var
  only.
- A7 needs A8's `MemberLeft` kind for G4.

## 6. Release order inside 0.5

1. Waves 1 and 2 can ship behind no flag: they are class L or R and correct on
   rules 3.
2. A6 changes visible behaviour (no answer-locally fallback, no silent Claude
   Code). It ships with its chat rows and the What's new entry in the same
   PR.
3. A7 and A9 ship together: new workspaces on rules 4, existing ones by the
   Owner's switch.
4. 0.5 is cut when A9, B4, C5 to C8 and C9 have merged, the pinned-0.4 tests
   (L6 to L8, H16, V1, V2) pass, and the full security pass over A7, B2, B4,
   C3 and C4 has no open finding.

## 7. Deferred to 0.6

- The person's private cross-device state: read marks, Hide for me, the
  per-chat model choice and push preferences in a sealed `personal-state` log
  in the Personal workspace (D13).
- One passkey over a batch digest, for Clean up and Approve & apply (D17).
- The events `place` split (`user_version` 3) and the deletion of legacy
  sources, the 0.3-era sync tables and the dormant local account seed
  (`user_version` 4).
- Live text over relay presence, which would first need expiry, delete and
  signed frames on the relay (D6).
- Durable relay mailboxes (0.5 posts again after a restart).
- Two identity hardening items carried from v2, unlikely in practice: the
  directory's issued-at clamp and a cost cap on acceptance checks.
- Opt-in automatic box upgrades; a setup assistant agent with a confirm step;
  inviting from the phone; admitting several boxes at once.
