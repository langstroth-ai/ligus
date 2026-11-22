# The Review screen and "Approve & apply" (design note)

Status: design only, not implemented. Target: Ligus 0.2, inside today's
layout (the rail card opens a full-canvas Review view in the main area; no
navigation change). Ligus is short for *ligustica*, the Italian honeybee.

Reviewing a proposal today means a 340 px rail card that stacks badges, the
body, an inline diff, the thread, votes, Approve/Reject, Implement…, Apply on
host and Send to agent to revise. This note specifies one Review screen and,
above all, the one place where it changes governance: a single button that
approves and applies when your vote is the one that completes quorum.

Decided by the owner: one "Approve & apply" button only when your vote
completes quorum; otherwise the button reads "Approve (1 more needed)".
Votes stay whole-proposal; no partial accept of someone else's proposal.

## Decisions

Owner, 2026-11-22:

- One "Approve & apply" button only when your vote completes quorum;
  otherwise "Approve (N more needed)".
- **Two simultaneous last votes: accepted**, with a clear message to the
  person whose apply didn't count (section 3.2, step 8).
- **The 600 ms click guard: accepted** (section 3.4).
- **One passkey ceremony, not two**, on human-gated changes: the owner found
  two prompts "heavy". Section 3.5 specifies one assertion that covers both
  signed events without weakening what either proves.

## 1. What exists today (code-verified)

Line numbers are as of `origin/main` at `001a9b8a6`.

- **Quorum** (`crates/ligus-core/src/proposals.rs`):
  `qualifying_approvals()` (:341-347) counts approvals at or above
  `approval_role_floor`, excluding the author; `is_quorum_met()` (:350-352)
  is `required_approvals > 0 && qualifying >= required`;
  `has_qualifying_rejection()` (:363-368) means any reject at or above the
  floor (the author's included) rejects; `recompute_status()` (:375-387);
  `ready_to_apply()` (:358-360) is `status == Approved && superseded_by.is_none()`.
  The latest vote per actor wins (`cast_vote`, :302-306).
- **Votes bind content.** `vote_on_proposal`
  (`crates/ligus-runtime/src/chat_service.rs:2738-2780`) signs a
  `ProposalVoteCast` naming `votes::diff_digest(p)`; the fold
  (`authz/mod.rs:734-765`) denies a vote for other content, a vote not signed
  by a certified device of its voter, and a delegated host's vote without
  presence, and counts it at the voter's real role (`events.rs:2137-2143`).
  An upsert that changes the digest clears every vote (`authz/votes.rs:70-71`).
- **Approving already applies one kind.** A workflow proposal is added by the
  vote that approves it (`chat_service.rs:2771-2778`, `apply_workflow_proposal`
  :2163-2189). Every other kind needs a person to run Implement. The user
  docs' "Approving never applies anything on its own" is therefore not quite
  true for workflows; fix the sentence when this ships.
- **Implement** (`app/src/lib.rs:6881-7101`, `implement_proposal_targeted`)
  refuses: already applied, rejected, superseded, an open apply request,
  not `ready_to_apply`, a `guard::human_gate_reason` (lockdown, dearth mode on
  protected paths: needs a human's own signed vote), and `authz.blocks` (a vote
  or decision still waiting to sync). Then: workflow → add the definition;
  file diff with a repo → `repos::apply::apply_to_chat_branch` on this device's
  checkout (compare-and-swap of `ligus/chat/<sid>`); legacy file diff →
  `git apply` and commit; structured command → exactly the argv, sandboxed,
  no network, no model, marked applied only on exit 0; anything else → the
  instruction from `review::implement_instruction` sent to an agent turn.
- **What will run.** `review::implement_plan` (`crates/ligus-runtime/src/review.rs:76-104`)
  is the one function both the preview and the runner use
  (`executed_instruction`, `executed_argv`). The preview is
  `web/src/components/ImplementPreview.tsx` (`PlanBody`, :76-142).
- **Apply on a host.** `request_apply` (`chat_service.rs:2250-2271`) files a
  `ProposalApplyRequested`; only one request may be open per proposal, the
  host must be a trusted non-phone device (`authz/mod.rs:835-878`), and only
  that host fulfils it, once (`crates/ligus-runtime/src/apply_host.rs`).
- **Presence.** A vote on a workflow proposal or a human-gated proposal needs
  the passkey, and so does a self-apply of a human-gated one
  (`authz/rules.rs:84-99`).
- **UI.** `ProposalCard` (`web/src/components/RightRail.tsx:1291-1757`): vote
  buttons :1604-1624 (quiet once quorum is met), Implement… :1627-1640 (shown
  on `quorumMet && status !== "applied"`, which includes a Rejected proposal
  that still has quorum: the backend refuses it, the button shouldn't show),
  host picker :1694-1724. `vote_proposal_sync` (`app/src/lib.rs:6306-6333`)
  returns a *locally projected* proposal, not a fresh fold.

## 2. The Review screen

Opened from the rail card's **Review ⏎**, `⌘⇧R`, the session's Changes tab
and the Inbox. It takes the main canvas; Esc returns to where you were. The
rail card shrinks to title, quorum chip, `+N −N` and **Review ⏎**.

Three columns, each optional on narrow windows (they stack):

1. **Files**: each file with `+N −N`, a viewed mark (local, per device,
   keyed by `(proposal id, diff digest, path)` so a new version resets it), and
   config-impact marks.
2. **Diff**: one component (`ReviewDiff`, on the Monaco diff already
   lazy-loaded by `DiffView.tsx`), unified or side by side, with line comments
   from [proposal-line-comments.md](proposal-line-comments.md) and "v2 of 2 ·
   what changed since v1" (an interdiff against `parent_id`).
3. **Decision**: quorum progress ("1 of 2 approvals · admin and above"), each
   voter with their role, the badges (Config impact, Human gate, Presence
   required), **What will run** (section 4, inline, always visible), the host
   picker, and the actions.

Keys: `j`/`k` file, `n`/`p` hunk, `c` comment, `v` viewed, `a` approve,
`r` request changes, `⇧A` approve & apply (section 3.4).

Calm rules: no new banner; the decision column shows only what applies to
this proposal; the viewed marks and file list are quiet text, not chips.

## 3. "Approve & apply"

### 3.1 When the button appears

Let `p` be the proposal in this device's fold *now*, `me` this device's
author, and `counts = role_of(me) ≥ p.approval_role_floor && me != p.author`.

The decision column shows exactly one primary vote button:

| Condition (all evaluated on the current fold) | Primary button | Line under it |
|---|---|---|
| Not votable (settled, superseded, your role can't vote, an alternative in an open set) | none | the reason, as today |
| Votable, `counts`, `qualifying + 1 ≥ required`, no qualifying rejection, `implement_gate(p, after_my_vote) == Ok`, plan loaded | **Approve & apply** | "Your approval completes quorum (2 of 2), then this runs:" followed by What will run |
| Votable, `counts`, more approvals needed | **Approve (N more needed)**, N = `required − qualifying − 1` | none |
| Votable, `counts` is false | **Approve** | "Your vote is recorded but doesn't count toward quorum: you wrote this proposal." or "…: votes from ‹floor› and above count." |
| Quorum already met (you're an extra voter) | **Implement…** stays primary; Approve goes quiet, as today | |

Reject is always a secondary button next to it, and "Request changes" is a
third. "Approve" without apply is always available from the button's menu
(`▾ Approve only`), so no one is forced to apply in order to approve.

`implement_gate(p, after_my_vote)` is the existing Implement gate (section 1)
evaluated on a copy of the fold with this device's vote added: not
superseded, no open apply request, no `human_gate_reason` that this vote
does not satisfy, not `authz.blocks`, not an alternative in an open proposal
set, and, for "this device", a checkout of the proposal's repo. It is one
Rust function shared by the button state and the action, not a TypeScript
re-derivation.

Workflow proposals: the vote already adds the workflow (section 1), so for a
workflow the button is **Approve & add workflow** with the same rule, and the
action is just the vote.

### 3.2 What the button does

One IPC command, `approve_and_implement(session_id, proposal_id,
diff_digest, plan_digest, target_agent_id, host)`:

1. Reload the proposal from the store. If `diff_digest(p)` differs from the
   value the screen showed, stop: "This change was updated since you opened
   it. Nothing was approved."
2. Recompute the implement plan; if its digest (SHA-256 of the canonical JSON
   of `ImplementPreviewDto` with the chosen target and host) differs from
   `plan_digest`, stop: "What will run changed. Check it again. Nothing was
   approved."
3. Re-evaluate 3.1's condition on the fresh fold. If it no longer holds (a
   reject or another approval arrived), stop before voting and refresh the
   column: "Someone else voted meanwhile. Nothing was approved."
4. Cast the vote (`vote_on_proposal`). If the rules need presence for the
   vote or the apply, this is the **only** passkey ceremony (section 3.5).
5. Fold it. Re-run the full Implement gate on the **fresh fold**, never the
   locally projected copy that `vote_proposal_sync` returns.
6. Do a sync pull (bounded, 5 s) and re-check: no `ProposalApplyRequested`,
   no `ProposalApplied`, no qualifying rejection, still `ready_to_apply`.
7. Apply: `implement_proposal_targeted` with the previewed target, or
   `request_apply` for the chosen host. For a human-gated proposal the apply
   is always an apply request (section 3.5), naming this device or the chosen
   host, appended with the presence already gathered in step 4; no second
   ceremony.
8. If 5-7 refuse, the vote stands and the column says, in place (no toast):
   "You approved. Not applied: ‹reason›." with **Implement…** if it later
   becomes possible. Nothing retries on its own. When the reason is that
   someone else's apply folded first (the accepted race), the text is:
   "You approved. ‹Name› applied it first, so yours wasn't needed. Nothing
   else to do." If this device had already committed on its chat branch, it
   adds: "This device's copy of the commit was set aside." (the commit is
   left on a `ligus/set-aside/…` ref, never on the chat branch).

The vote and the apply are two signed events, as today. Without a human gate,
nothing new goes on the wire. With one, the only wire change is the additive
presence set of section 3.5; no wire id changes and the quorum rules are
untouched. "Approve & apply" is a client-side sequence with stricter checks
than two clicks.

### 3.3 The exact text of "What will run"

Built from `implement_plan`, so the words name what the runner does.

| Plan | Text |
|---|---|
| `applyDiff`, this device, workspace repo | **Applies the diff to the chat's branch** (`ligus/chat/…`) in this device's checkout of ‹repo›, as one commit. No model is involved. |
| `applyDiff`, legacy (no repo) | **Applies the diff in the chat's worktree** with `git apply` and commits it. No model is involved. |
| `argv` | **Runs exactly** `‹argv, escaped by showArg›` — in the chat's worktree‹ (cwd)›, sandboxed, with no network. No model is involved. It counts as applied only if it exits 0. |
| `instruction` | **Sends this to ‹@agent›**, which decides how to carry it out with its tools (‹runtime›, ‹permission tier›, sandbox ‹executor›, network ‹egress or "none"›): the instruction text in full. |
| `instruction` with `legacyCommand` | as above, plus the existing warning "No exact command was filed: an agent interprets this instruction." |
| `addWorkflow` | **Adds the workflow definition** ‹name›. It doesn't start a run. |
| Apply on a host | **Asks ‹host› to apply it once**, on its checkout, and report back. Nobody else applies it meanwhile. |

Always followed by the badges that apply ("Human gate applies", "Presence
(passkey) required") and, when a passkey is needed: "You'll confirm once with
your passkey. It covers your approval and this apply, and nothing else."

### 3.4 Keys and accidental activation

`a` approves (as a vote, never applies). `⇧A` focuses the Approve & apply
button and shows "Press ⏎ to approve and apply"; ⏎ then runs it. A mouse
click runs it directly, because the plan is on screen next to the button.
The button is disabled until the plan has loaded and for 600 ms after the
plan or the quorum line last changed, so a late re-render can't put a
different action under a moving pointer.

### 3.5 One passkey for the vote and the apply

Today each event that needs presence carries its own WebAuthn assertion in
its actor stamp (`ActorStamp.presence`, `crates/ligus-core/src/identity.rs:148-151`),
over a challenge computed from that event alone (`presence::challenge`,
`crates/ligus-core/src/presence.rs:245-270`: workspace, session, event id,
lamport, payload, config frontier, account; **not** the actor stamp), and
the fold checks it in `passkeys::verify_stamped` (`authz/mod.rs:575-610`).
A human-gated proposal needs presence on the vote and on a self-apply
(`authz/rules.rs:84-99`); an apply **request** carries the person's presence
and the host that fulfils it needs none ("the request carried the person's").

**Shape: a presence set of exactly two events.**

1. The client drafts both events first, fully: the `ProposalVoteCast`
   (approved, naming the shown digest) at lamport *L*, and a
   `ProposalApplyRequested` at *L + 1* naming the shown digest, the proposal's
   repo, the expected base commit, and the host: **this device** for
   "Apply on This device", or the chosen host.
2. It computes `c1 = challenge(vote)`, `c2 = challenge(request)` and the set
   challenge
   `S = SHA-256("ligus-presence-set-v1\0" ‖ u32le(2) ‖ c1 ‖ c2)`
   (a **new** frozen context; the existing `ligus-presence-v1\0` is untouched).
3. One ceremony signs `S`. The passkey sheet's title is "Approve and apply
   ‹title›".
4. Both envelopes carry the same assertion plus an additive actor-stamp field
   `presenceSet: [c1, c2]` (hex). The device signs each envelope as usual; the
   preimage covers the actor stamp, so the set is signed by the device too.
5. The vote is appended; then steps 5-6 re-check; only then is the request
   appended. If the re-check refuses, the drafted request is dropped from
   memory, never stored or sent.
6. This device's own apply-host loop (`apply_host::pending`, which picks up
   requests naming this device) fulfils the request once, with its usual
   claim, judge, apply and report steps. Non-gated proposals keep the direct
   `implement_proposal_targeted` path.

**Verification (fold, every device).** In `verify_stamped`, when the stamp
has `presenceSet`:

- it has exactly 2 entries, distinct, each 32 bytes;
- this event's own `challenge(env)` is one of them;
- the assertion verifies over `S` computed from the listed entries, by a
  credential of the stamped account, with user verification, as today;
- the event is a `ProposalVoteCast` with `approved: true` or a
  `ProposalApplyRequested`; any other kind carrying a set is denied
  ("a presence set covers only an approval and its apply");
- for a request, the fold additionally requires that the same account's
  latest vote on that proposal is an approval naming the same digest and
  carrying a `presenceSet` that contains this request's challenge. So a set
  can't pair a request with someone else's vote, or with a different
  proposal's.

**What each signature still proves.** The device signature on each envelope
is unchanged. The passkey assertion proves that the person approved this
exact pair of events: each event's challenge commits to its full payload,
event id and lamport, and `S` commits to both. An assertion can't be moved to
a third event (its challenge isn't in the set), the set can't be extended
(`S` fixes *n* = 2 and both entries), and replaying an event is refused by
event-id dedup as today.

### 3.6 Other surfaces

- **Phone:** never applies (the phone tier can't, and Implement runs on a
  computer). Its sign sheet uses the same "completes quorum" calculation for
  its text (see the iPhone app's read-first note).
- **`lt`:** may adopt the same rule later; until then it keeps separate
  approve and apply keys.

## 4. Tests (executable, required before merge)

| # | Test | Kind |
|---|---|---|
| T1 | Property: for random proposals (required 1-4, floor, author, existing votes incl. rejects, my role), the button state from 3.1 equals "my vote alone would flip `ready_to_apply` and `implement_gate` passes", computed independently from `ActionProposal`. | proptest (Rust) |
| T2 | The button never says Approve & apply when I'm the author, below the floor, there's a qualifying reject, an open apply request, a human-gate reason my vote can't satisfy, `authz.blocks`, or an open proposal set. | unit |
| T3 | Digest changed between render and click → nothing signed (no `ProposalVoteCast` in the store). | integration (`app` test harness) |
| T4 | Plan changed (target agent removed, egress changed) between render and click → nothing signed. | integration |
| T5 | Another member's reject folds between click and step 5 → my vote is cast, nothing applies, the column says "You approved. Not applied: …". | integration with two devices on the test relay |
| T6 | Two members each see Approve & apply on a 2-of-3 proposal with 1 approval and click together → at most one `ProposalApplied` folds on every device; the loser's column says not applied; neither retries. | integration, two devices, randomized delays (100 runs) |
| T7 | Human-gated proposal: exactly one passkey ceremony for Approve & apply; cancelling it signs and appends nothing. | integration with the stub authenticator |
| T8 | Workflow proposal: "Approve & add workflow" casts one vote and adds the definition once; an older peer reaching quorum does not add it twice. | unit |
| T9 | The "What will run" text for each plan kind is produced from `implement_plan` (snapshot test per kind), and the runner's `executed_argv`/`executed_instruction` equal the plan shown. | unit + snapshot |
| T10 | Implement… is not shown for a Rejected proposal that still has quorum. | web unit (vitest) |
| T11 | `⇧A` alone never applies; `⇧A` then ⏎ does; the button ignores clicks for 600 ms after the plan changes. | web unit + Playwright |
| T12 | Presence set: golden vectors for `S` (new context, two challenges); a set of 1 or 3 entries, a duplicate entry, an event whose own challenge isn't listed, or an assertion over `c1` alone are each denied. | unit + golden |
| T13 | A set assertion copied onto a third event (another vote, a `DeviceRevoked`, a rules change) is denied; an apply request whose set doesn't match the same account's approving vote on the same proposal and digest is denied. | unit (forged-envelope harness) |
| T14 | Re-check refuses after the vote (a reject folded): the drafted request never reaches the store or the relay. | integration |
| T15 | The request names this device; `apply_host::pending` on this device fulfils it once, and a restart mid-apply doesn't apply twice. | integration |
| T16 | The second last voter whose apply lost sees exactly the step-8 text, and a commit already made is on a set-aside ref, not the chat branch. | integration, two devices |

## 5. Adversarial assessment

Stance: the adversarial architecture reviewer (governance lens). The first
pass was run on a draft that defined the button as "vote, then if the
returned proposal shows quorum, implement". The note above contains the
revisions.

### First pass: Revise

1. **Acting on a projected copy** (High, high). `vote_proposal_sync` returns
   `proposal.cast_vote(approval)` on a local copy. A reject from another
   member that had already folded is in that copy, but one that arrives during
   the call isn't, and the copy ignores `authz.blocks`. Applying on that copy
   could run a rejected change. *Revision: steps 5-6 on the fresh fold, test T5.*
2. **"What will run" not bound to the click** (High, medium). The preview
   is fetched once; the target agent's runtime, sandbox or egress can change
   before the click. *Revision: `plan_digest`, step 2, test T4.*
3. **Content not bound to the click** (High, medium). A re-upsert between
   render and click changes the diff; the vote would then bind the *new*
   digest (the core computes it at vote time), so the person approves
   content they never saw. *Revision: `diff_digest` from the screen, step 1,
   test T3.*
4. **Double apply** (Medium, high). Two last voters both see the button.
   Today two people can also both press Implement, but the combined button
   makes it the default path. *Revision: step 6's pull and re-check, test T6;
   residual risk below.*
5. **Button state re-derived in TypeScript** (Medium, medium). The web layer
   would drift from `ready_to_apply` (it already shows Implement on a
   Rejected proposal). *Revision: one Rust `implement_gate`, tests T1, T10.*
6. **Keyboard activation** (Medium, medium). A single key that both votes and
   runs code is too easy to hit. *Revision: 3.4, test T11.*
7. **Workflows already approve-and-apply** (Low, high). Unstated in the
   draft. *Revision: 3.1's workflow row, test T8, the docs fix in section 1.*

### Second pass

#### Decision

**Approve with risks.**

#### Claimed invariants

- G1. Nothing is applied that the current fold doesn't consider
  `ready_to_apply` with the human-gate and sync-block checks passing.
- G2. The vote binds the content shown on screen, and the run is the plan
  shown on screen.
- G3. No quorum or role rule changes: same events, same quorum, same
  presence requirements; the only fold addition is verifying a presence set
  (section 3.5).
- G4. The button appears only when this person's vote is what completes
  quorum.
- G5. Approving is always possible without applying.

#### Hidden assumptions

- The fold is the source of truth for quorum at the moment of the check; a
  vote signed elsewhere and not yet synced is invisible. This is true of
  Implement today.
- `implement_plan` is deterministic for a given session state (true: pure
  function of the session and proposal).
- The sync pull in step 6 reaches the relay; offline, it times out and the
  apply proceeds on local knowledge, as Implement does today.

#### Counterexamples considered after revision

| Claim | Sequence | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| G1 | A rejects offline; B's Approve & apply completes and applies; A syncs later | Not applied | Applied; A's later reject folds after `ProposalApplied` (Applied is terminal) | Medium | High |
| G4 | Two last voters click within the pull window | One applies | One `ProposalApplied` folds; the other device may already have made a commit on its own chat branch | Low-Medium | Medium |
| G2 | The agent's runtime changes between step 2 and step 7 | Runs the shown plan | Window is the length of one vote (passkey included); runner re-reads the plan | Low | Medium |

The first row is not new: Implement has the same property, and quorum is
defined over the votes the applier can see. It is listed so nobody believes
the combined button closes it.

#### Required tests

T1-T16. T1, T5, T6, T12 and T13 are the governance proof.

#### Simpler alternatives

Keep two buttons and make the second one appear in place after the vote
("You approved. Quorum met: Implement…"). It is one more click, needs no new
IPC and has no double-apply amplification. If T6 shows the double apply
happens in practice, fall back to this.

#### Residual risks

- Offline rejects can still lose to an apply (unchanged from today).
- Two simultaneous last voters can both start applying; only one counts, and
  the other may leave a local commit to clean up. Rare in practice; visible.

### Third pass: one passkey for both events (section 3.5)

Re-run on the owner's change. The first form considered was "one assertion
over the vote's challenge, and the apply references it".

#### First form: Revise

1. **Apply events can't be pre-signed** (High, high). A self-apply's
   `ProposalApplied` names the commit and tree it produces, which don't exist
   at ceremony time, so its challenge can't be computed in advance.
   *Revision: the apply is an apply request (known in full in advance) that
   this device fulfils as a host, which already needs no presence of its own.*
2. **Referencing is not binding** (High, medium). An apply that merely names
   the vote's event id would let a compromised device file any request for
   any host while the person saw "This device". *Revision: the request's own
   challenge is inside the signed set (3.5, step 2), test T12.*
3. **Set reuse** (High, medium). A set without a kind restriction lets a
   compromised client pair the visible approval with a hidden governance event
   (a revocation, a rules change). *Revision: only an approving vote and an
   apply request may carry a set, and the request must match the same
   account's vote on the same proposal and digest; test T13.*
4. **Orphaned request** (Medium, medium). If the re-check fails after the
   vote, a signed, presence-carrying request exists. *Revision: never stored
   or sent (step 5), test T14. The fold would still refuse it unless the
   proposal is approved, which is what the person agreed to.*

#### Decision

**Approve with risks.**

#### Claimed invariants

- P1. One ceremony authorises exactly two events: this person's approval and
  one apply request for the same content.
- P2. Neither event's authorisation is weaker than today: each still has its
  device signature, and the passkey covers its full challenge.
- P3. No existing signing context, wire id or field changes; the set is a new
  context and an additive stamp field.

#### Hidden assumptions

- Every device runs a build that knows presence sets (owner: no older clients
  to support). An older build would deny both events for "presence required".
- Applying through the request path on this device gives the same result as
  the direct self-apply (both use the chat's worktree on the expected base;
  T15 pins it).

#### Counterexamples considered

| Claim | Sequence | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| P1 | Compromised device shows "Apply on This device", drafts a request naming another host | person's choice | the passkey sheet can't show the host; the request is the one signed, so the wrong host would be authorised, as a compromised device could already ask for any single-event ceremony today | Medium | Medium |
| P1 | Vote folds, request is refused (base commit moved) | vote stands, nothing applied | as expected; the person asks again with a new ceremony | Low | High |
| P2 | Assertion lifted onto a second request for the same proposal | denied | denied: its challenge isn't in the set; one open request per proposal anyway | Low | High |

#### Residual risks (third pass)

- A compromised desktop can still misrepresent what a passkey ceremony
  approves; true of every ceremony today, and the set doesn't widen it beyond
  one approval plus one apply request.
