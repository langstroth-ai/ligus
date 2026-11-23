# Line comments on proposals (design note)

Status: design only, not implemented. Target: Ligus 0.2 (desktop), then `lt`
and the iPhone app. Ligus is short for *ligustica*, the Italian honeybee.

A reviewer wants to point at one line of a proposed change and say something
about it, and every other reviewer should see that comment, encrypted end to
end like the rest of the workspace. This note decides how the comment travels
and what has to change in the fold so it can be trusted.

**Decision taken here:** reuse the existing refinement thread
(`SessionEvent::ProposalRefined`) and add an optional line anchor to
`ProposalRefinement`. No new event kind, no renamed wire id, votes stay
whole-proposal. The fold gets three fixes it needs anyway (section 4), because
today the thread is not trustworthy enough to carry line comments.

## Decisions

- **Owner, 2026-11-22: line comments may require an updated client.** Nobody
  uses Ligus actively yet, so this design does not carry compromises for
  older builds: every device that shows, writes or folds line comments runs
  0.2 or later. What stays: new fields are additive, no existing wire id or
  field is renamed, and no signing context changes.
- Reuse the refinement thread with a line anchor (preferred by the owner);
  votes stay whole-proposal.

## 1. What exists today (code-verified)

Line numbers are as of `origin/main` at `001a9b8a6`.

| Piece | Where | What it does |
|---|---|---|
| Thread event | `crates/ligus-core/src/events.rs:163-166` | `ProposalRefined { proposal_id: Uuid, refinement: ProposalRefinement }`, wire id `proposalRefined` (variant fields stay snake_case; the struct is camelCase). |
| Comment struct | `crates/ligus-core/src/proposals.rs:53-66` | `ProposalRefinement { id, actorId, role, text, requestChanges (default false), createdAt (default) }`. No size limit. |
| Fold | `events.rs:954-959`, `proposals.rs:326-331` | `add_refinement` appends, deduplicated by comment id. No-op for an unknown proposal. |
| Upsert | `events.rs:921-941` | `ProposalUpserted` replaces the stored proposal **including `refinements`** (`*slot = proposal.clone()`). `normalize_upsert` (`authz/votes.rs:63-83`) resets votes and status, not the thread. |
| Authoring | `crates/ligus-runtime/src/chat_service.rs:2785-2820` `refine_proposal`; desktop `app/src/lib.rs:6344-6371` `refine_proposal_sync`; box `crates/ligus-cli/src/serve/api.rs:1355-1377` `POST /v1/proposals/{id}/comments` | Takes the caller's actor id and role, `id = Uuid::new_v4()`. The desktop trims and refuses empty text. The box signs as the box. |
| Who may post | `authorization.rs:250-253` (Contributor floor when *this device* authors); `authz/mod.rs:536-548` (fold: membership only); `authz/agent.rs:42` (agent: `refinement.actor_id == me`); `authz/phone.rs:23` (phone tier allows it) | At fold, a person's comment is checked only for membership: nothing checks that `actorId` is the signer, and `role` is not rewritten to the signer's real role (votes get both, `events.rs:2137-2143`). |
| Digest | `authz/votes.rs:24-55` `diff_digest` | Covers kind, diff, changed files, workflow, command, id, repo, base commit, attachment. **Not** title, body, thread, `parent_id`, `superseded_by`. A comment can never invalidate a vote. |
| Revise loop | `app/src/lib.rs:6376-6525` `request_proposal_changes` / `run_revision_turn`; box `api.rs:1440-1500` | Collects change requests (or every note) as `- {text}` lines into an instruction, runs a turn, upserts the child with `parent_id` and **re-upserts the parent** with `superseded_by`. |
| Display | `ligus-proto/src/lib.rs:156-171` `RefinementDto`; `web/src/components/RightRail.tsx:1797-1882` `RefinementThread`; box `comment_dto` (`api.rs:457-467`) | Flat list, no anchors. |
| Transport | `crates/ligus-runtime/src/sync_engine.rs:497-511`; `crates/ligus-core/src/e2ee.rs:195-211` | The whole signed envelope (kind, session, payload) is sealed with ChaCha20-Poly1305 under the epoch key. The relay sees the workspace id, epoch, `kid`, nonce, ciphertext size, an HMAC idempotency key and timing. |
| Forward compatibility | `events.rs:570-577`, `envelope_wire.rs:1-19, 206-208`, `crypto.rs:102-127` | Unknown kinds fold to `Unknown`; unknown fields are ignored (no `deny_unknown_fields` on any event or proposal type); received envelopes keep their exact bytes and verify over them. |
| Limits | `event_store.rs:44-55` `MAX_ENVELOPE_BYTES` (1 MiB − 16 KiB); `sync_engine.rs:29-31` 4 MiB push body | Nothing per comment, nothing per thread. |

## 2. Options

**A. Reuse `proposalRefined` with an optional anchor (chosen).**
Two additive fields on `ProposalRefinement`. The thread's projection,
authz, DTO and the "Send to agent to revise" loop already exist and keep one
source of truth.

**B. A new event kind, for example `proposalLineCommented`.** It needs a
golden vector (`golden_coverage.rs`), new authz rules, new projection code
and a new DTO, and the revise loop would have to read two sources, for no
gain once older builds are out of scope.

**C. Encode the anchor in the text** (`src/a.rs:42: …`). No schema change,
but the anchor becomes unauthenticated free text that any author can fake,
and every client has to parse it. Rejected.

## 3. Shape

### 3.1 Wire (additive only)

```rust
// crates/ligus-core/src/proposals.rs, inside ProposalRefinement
#[serde(default, skip_serializing_if = "Option::is_none")]
pub anchor: Option<LineAnchor>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub reply_to: Option<Uuid>,          // another refinement's id, same proposal

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineAnchor {
    pub diff_digest: String,         // the proposal content the comment was made on
    pub path: String,                // the file path as it appears in the diff
    pub side: DiffSide,              // "old" | "new"
    pub line: u32,                   // 1-based line number on that side
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,       // inclusive; a range on the same side
    pub excerpt_sha256: String,      // hex SHA-256 of the anchored line(s), LF-joined
}

#[serde(rename_all = "camelCase")]
pub enum DiffSide { Old, New }
```

Rules:

- No existing field changes name, type or default. `proposalRefined` keeps
  its wire id. No signing context changes (`hive-envelope-v3\0` stays).
- An event without `anchor` means what it means today: a general comment.
- `diff_digest` is the value of `votes::diff_digest(p)` the author's device
  saw. It makes an anchor say *which content* it points at, the same way a
  vote does, so a later revision can't silently re-point it.
- `excerpt_sha256` lets every reader check that the line it is about to draw
  the comment under is the line the author saw, even if two clients parse a
  diff slightly differently.
- `reply_to` makes a thread under one line. A reply carries no anchor of its
  own; it inherits the parent's.
- Resolving a thread is **not** in this note. A later additive field
  (`resolves: Option<Uuid>`) can add it; until then a reply "Done" is the
  convention.

### 3.2 Rendering

A comment is **current** when `anchor.diff_digest == diff_digest(p)` and the
line at `(path, side, line..=end_line)` in `p.diff` hashes to
`excerpt_sha256`. It is drawn under that line.

Otherwise it is **outdated**: drawn in the thread with "On an earlier version
of `path`, line N" and the excerpt text taken from the earlier proposal if the
client still has it. It is never drawn under a different line.

A revision is a new proposal (`parent_id`), so comments stay on the version
they were made on. The Review screen's version history shows them on that
version, and "what changed since v1" shows outdated comments next to the
interdiff.

### 3.3 Authoring and batching

- Comments are **drafted locally** (per device, not synced) and sent together
  with "Send comments" or with "Request changes". Each comment is its own
  `proposalRefined` event, so one failed push never loses the others and the
  existing dedup-by-id covers retries.
- "Request changes" sets `requestChanges = true` on every comment in the batch.
  `request_proposal_changes` adds the anchor to each line of the instruction:
  `- src/scheduler.rs:43 (new): use the forecast high, not current`. The
  anchored excerpt is **not** pasted into the instruction (the agent has the
  diff; pasting reviewer-chosen text into a prompt widens injection).
- Before sending, the client recomputes `diff_digest` from the proposal it
  has. If the proposal changed while the drafts were open, drafts on changed
  lines are kept as drafts and marked "the change was updated; check these
  before sending".
- The phone has no comment `PhoneAction` today (the box posts comments as
  itself, `api.rs:1355-1377`). Line comments from the phone wait for that
  action; the phone shows anchored comments read-only first.

### 3.4 Limits

- `text`: at most 8,000 characters (`MAX_REFINEMENT_CHARS`), checked when
  authoring and at fold (section 4). An anchored comment with empty text is
  refused.
- `path`: at most 1,024 bytes, no NUL, must appear as a file in `p.diff`
  when authored (not checked at fold: the fold doesn't parse diffs).
- `end_line - line` at most 500.
- Per proposal, the projection keeps at most 2,000 comments; beyond that the
  fold drops new ones and the thread says "Some comments are not shown". The
  envelope cap already bounds each event.

## 4. Fold changes the thread needs first

These close gaps that exist today. They matter more once comments carry a
location people will act on.

1. **A re-upsert never rewrites the thread.** In the `ProposalUpserted` arm,
   when a slot exists, keep `slot.refinements` and ignore the upsert's. Today
   a re-upsert (the revise loop does this to set `superseded_by`) replaces the
   whole thread with the copy its author had, so a comment that author hadn't
   yet seen (a concurrent comment) disappears on every device that folds the
   upsert after it. It also lets an upsert insert comments under any name. For a first upsert (no slot), keep only comments
   whose `actorId` is the upsert's author; a compaction seed carries the
   thread through its own seed path and is unaffected (to verify, test T7).
2. **A comment is by its signer.** At fold, a person's or phone's
   `proposalRefined` must name the signer's actor (`refinement.actor_id ==
   author`), as votes must; `role` is overwritten with the signer's real role
   at that position, as for votes (`events.rs:2137-2143`).
3. **The authoring floor holds at fold.** Contributor and above (the
   existing `min_role_for`), plus agents for their own comments and the
   chat's primary runtime, as today. Viewers' comments are denied at fold, not
   only when their own device authors them.
4. **Bounds at fold:** text length and the anchor bounds of 3.4.
5. **`reply_to` must name a comment on the same proposal** already in the
   fold; otherwise the comment is shown as a top-level comment (not denied:
   order of arrival can put a reply before its parent on a partial sync, and
   the fold re-runs in canonical order).

Fixes 1-4 ship in the same release as anchors; all devices fold the same
rules.

## 5. What does not change

- Votes stay whole-proposal: `ProposalVoteCast` is untouched and a comment has
  no effect on `qualifying_approvals`, `has_qualifying_rejection`, status or
  the human gate.
- No comment ever blocks Approve. "Request changes" is a comment plus the
  revise loop; it is not a reject.
- The relay sees nothing new beyond one more envelope per comment (size and
  timing, as for any event).

## 6. Tests (executable, required before merge)

| # | Test | Kind |
|---|---|---|
| T1 | An anchored `proposalRefined` round-trips; an old-shape struct (no `anchor`) decodes; a golden vector with an anchor is added to `tests/golden/envelopes_v3.jsonl`. | unit + golden |
| T2 | An anchored event received from another device verifies over its received bytes and re-pushes byte for byte (`ingest_stores_the_received_bytes` style). | unit |
| T3 | Property: for any permutation of {upsert v1, N comments from several devices, re-upsert with `superseded_by` made before some comments were seen, upsert of child}, every device ends with the same thread, no comment is lost and every comment keeps its anchor. | proptest over orderings |
| T4 | A `proposalRefined` whose `actorId` isn't the signer is denied at fold; one with a forged `role` is stored with the signer's real role. | unit |
| T5 | A Viewer's comment signed by the Viewer's own device is denied at fold on a second device. | unit |
| T6 | Text over 8,000 characters, an empty anchored comment and an anchor range over 500 lines are denied; 2,001 comments leave 2,000. | unit |
| T7 | A compaction seed carrying a thread restores it; a first upsert carrying comments by someone else drops them. | unit |
| T8 | A comment whose `diff_digest` is stale, or whose excerpt hash doesn't match, renders as outdated and never under another line. Fuzz the diff parser with the anchor resolver. | unit + fuzz |
| T9 | A reply that arrives before its parent renders top-level, then nests once the parent folds; a `reply_to` naming another proposal's comment stays top-level. | unit |
| T10 | "Request changes" produces one instruction line per anchored comment with `path:line (side)` and no excerpt text. | unit |
| T11 | Two devices of one reviewer send the same draft batch twice (retry after a lost ack): one copy of each comment. | integration with the test relay |

## 7. Adversarial assessment

Stance: the adversarial architecture reviewer. The first pass was run on the
first draft of this note; the note above already contains the revisions it
required. The second pass is the verdict.

### First pass: Revise

The first draft added `anchor` and stopped there. Findings:

1. **A re-upsert erases comments** (High, confidence high). Sequence:
   device A posts an anchored comment; device B, which hasn't folded it yet,
   runs "Send to agent to revise", which re-upserts the parent with
   `superseded_by` from its projection; every device folds B's upsert after
   A's comment and `*slot = proposal.clone()` replaces the thread. Expected:
   A's comment survives. Actual: gone on every device. *Revision: fold fix 1,
   test T3.*
2. **Forged authorship** (High, high). A member's device signs a
   `proposalRefined` with `actorId` of the Owner; the fold checks membership
   only, so it shows as the Owner's comment, and the revise loop feeds it to the
   agent as the Owner's instruction. *Revision: fold fix 2, test T4.*
3. **Upsert injects comments** (Medium, high). An agent's own re-upsert can
   carry any `refinements` (the agent tier checks `approvals.is_empty()`
   only). *Revision: fold fix 1, tests T3 and T7.*
4. **Anchor without content binding** (Medium, medium). A bare `(path, line)`
   re-points silently after a revision that shifts lines. *Revision:
   `diff_digest` and `excerpt_sha256`, test T8.*
5. **Resource exhaustion** (Medium, medium). No limit on comment size or
   count; a misbehaving agent can grow every reviewer's projection without
   bound. *Revision: section 3.4 and fold fix 4, test T6.*
6. **Prompt widening** (Low, medium). Pasting excerpts into the revise
   instruction lets a reviewer choose text the agent will read as content.
   *Revision: path and line only, test T10.*

### Second pass (after the owner's decision that line comments may require an updated client)

#### Decision

**Approve with risks.**

#### Claimed invariants

- I1. An anchored comment is readable by exactly the workspace's key holders
  (it rides an ordinary sealed envelope).
- I2. The new fields are additive: no existing wire id, field or signing
  context changes, and signatures verify over the received bytes.
- I3. Every device converges on the same thread for the same event set, in
  any order.
- I4. A comment is attributed to its signer at the signer's real role.
- I5. A comment never changes what a vote binds, the quorum or what runs.
- I6. A comment is drawn under a line only if that line is the one its author
  saw.

#### Hidden assumptions

- Every device in the workspace runs 0.2 or later (owner decision). A device
  on an older build would show anchored comments without their line and fold
  the thread with the old rules; this is unsupported, not designed around.
- `diff_digest` computed on the author's device equals the fold's value for
  the same proposal (true today; pinned by `proposal_digest_v1.jsonl`).
- Clients parse a unified diff into the same `(path, side, line)`
  coordinates. Not guaranteed; the excerpt hash turns disagreement into
  "outdated" instead of a wrong line.
- Compaction seeds carry the thread through a path other than a normal
  upsert (to be confirmed by T7 before fold fix 1 lands).

#### Counterexamples considered after revision

| Claim | Sequence | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| I3 | A re-upsert made before two concurrent comments folds after them, in either order | both comments kept | kept (fix 1) | Low | High |
| I6 | Two parsers disagree on a rename-with-edit hunk | Comment under the right line | Shown as outdated on the client that disagrees | Low | Medium |
| I1 | A removed member's device posts a comment signed before removal, delivered after | Shown if before the removal watermark, as for every event | Same as today's membership rule | Low | High |
| I4 | A Viewer's device posts a comment | denied everywhere | denied at fold (fix 3) | Low | High |

#### Required tests

T1-T11 above. T3 and T4 are the ones that prove the thread can be trusted;
they must exist before the fields ship.

#### Simpler alternatives

Shipping anchors without the fold fixes would be smaller, but finding 1 loses
comments whenever a revision races a comment, and finding 2 lets a member
speak as the Owner to the agent, so the fixes are the minimum. Leaving out
`reply_to` is possible; it is kept because it costs one optional field and the
Review screen's design needs it.

#### Residual risks

- A device still on an older build sees a different thread; unsupported by
  decision.
- The relay learns that someone in the workspace sent N small events at
  time T (true of every event).
- Comments from the phone arrive only once a phone comment action exists.
