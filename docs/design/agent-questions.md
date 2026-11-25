# Agent questions: decisions an agent waits on (design note)

Status: design, owner decisions taken 2026-11-25 (section 13); not implemented. Target: Ligus 0.4. Surfaces: the Ligus
MCP server, the desktop, the phone apps, `lt` (ligus-term), the `ligus` CLI
and `ligus serve`. Ligus is short for *ligustica*, the Italian honeybee.

Citations are `file:line` into this repository at `origin/main` `3c1638ddf`.
When this note and the code disagree, the code wins.

## 1. The problem

An agent often reaches a point where one word from a person unblocks it:
"Postgres or SQLite?", "Delete the generated fixtures too?", "May I run
`cargo publish --dry-run`?". Prior art puts this in a small decision card
with buttons: the agent waits, the person taps once from wherever they are,
and the agent carries on.

Ligus has most of this already. It lacks four things:

1. **Who may answer.** Today it is anyone in the chat, and the first answer
   wins.
2. **What the answer does.** The card doesn't separate the agent's words from
   the effect of the answer.
3. **A consistent card** across the desktop, lt and the phone.
4. **Reach** to sub-agents and to agents Ligus didn't start.

This note extends what exists. It adds no new system.

## 2. What exists (code-verified)

- **The tool.** `ligus_ask_human` (`crates/ligus-runtime/src/ligus_mcp/tools.rs:13-49`,
  schema `ligus_mcp/schema/ligus_ask_human.json`, folded as `ligus_ask`,
  `tools.rs:53-58,85-138`).
  - Arguments: `question` (up to 2000 characters), `options` (up to 6, each
    up to 200 characters) and `timeoutSecs` (10 to 3600, default 600).
  - The schemas are frozen by a golden hash (`tools.rs:157`).
- **How the wait works.** `ask_human` (`ligus_mcp/server.rs:1126-1152`) →
  `post_ask_as` (`:1239`) records `FlowRecord::HumanAsk` signed as the agent.
  `wait_answer` (`:1270-1285`) polls the chat fold every 250 ms until an
  answer, the deadline or turn revocation, while `pause_counter`
  (`agent_turn.rs:199-260`) stops the idle timers.
  - On timeout it writes `HumanAsk{closed:true}` and returns
    `{"error":"no_answer"}`.
  - On an answer it returns `{answer, optionIndex, answeredBy}`.
- **Budgets.** `ligus_mcp/server.rs:50,69`: 3 asks per turn and 6 per cascade.
  `LIGUS_MCP_MAX_*` overrides them.
- **Records** (`crates/ligus-core/src/flow/mod.rs:298-300,447-512,712-732`):
  - `HumanAsk{askId, turnId, agentKey, question, options, expiresAtMs, closed, atMs, permission}`
    and `HumanAnswered{askId, answer, optionIndex, by, atMs}`.
  - Bounds come from `authz/bounds.rs:77-87`. Only the asking turn may close
    an ask, and the question is fixed once posted.
  - **The first answer in canonical order wins.** A closed ask takes none.
  - `AskState::is_open` ignores expiry.
- **Who signs.**
  - Only a `Human` actor signs `HumanAnswered` (`authz/mod.rs:293-298`).
  - `FlowRecorded` needs Contributor (`authorization.rs:228`), so a Viewer
    can't answer.
  - Phones may sign it (`authz/phone.rs:34`).
  - The agent tier lets an agent sign `HumanAsk` only for itself
    (`authz/agent.rs:54`).
  - Answers need no passkey: `HumanAnswered` is not a governance record
    (`authz/rules.rs:50`).
- **Permission escalations** (`crates/ligus-runtime/src/provider/acp.rs:624-684`):
  - Trigger: an ACP agent asks to run something above its tier.
  - The host posts a `HumanAsk{permission:true}` with "Allow once" / "Deny"
    (`escalate`, `LIGUS_ACP_ASK_TIMEOUT_SECS`, default 600).
  - **The grant is checked only on the host**: `answered_by_approver`
    (`ligus_mcp/server.rs:1435-1446`) accepts the host's person or the
    agent's owner, and any other first answer means deny.
  - Its wait does not watch turn revocation (`:1415`, `|| false`).
- **Surfaces.**
  - Desktop: `web/src/components/AgentAsks.tsx` shows a Markdown question,
    option buttons and a free-text field, and refetches every 4 s. It is
    mounted in the right rail (`RightRail.tsx:1830`) and the Inbox
    (`Inbox.tsx:44-56`, row "An agent asked you something",
    `needsYou.ts:98-100`). The desktop's `AskDto` has **no kind**
    (`web/src/lib/ipc.ts:1859-1875`), so a permission ask looks like a
    question there.
  - Phone: `GET /v1/chats/{id}/asks` (`crates/ligus-cli/src/serve/api.rs:871`),
    whose `ask_dto` has a `kind` (`:419-433`). Phones answer through
    `/v1/phone-actions` with `kind:"answer"`, so Face ID is asked on every
    answer (`docs/ios-app.md:764-780`; only `post` is exempt). The phone gets
    SSE `ask.pending` and a push (`serve/notify.rs:170`).
  - lt reads the serve API (separate repository).
- **Rendering.** `web/src/components/Markdown.tsx`: no raw HTML, links limited
  to `https:`/`mailto:` and opened only on click through `openExternal`,
  images never loaded. Names go through `clean_display_name`
  (`crates/ligus-runtime/src/display.rs`), which strips bidi, zero-width and
  control characters; bodies don't.
- **Claude Code turns Ligus starts** (`provider/claude_code.rs:331-404`):
  hooks are off (`disableAllHooks`), tools are pinned, and `Task`/`Agent`
  sub-agents inherit the turn's MCP credential and budgets (`:227-231`).
  There is **no `ligus hook` command** (`crates/ligus-cli/src/main.rs:2389-2724`).
- **Gaps** this note closes:
  - A host restart leaves asks looking open: nothing sweeps them, and
    `is_open` ignores expiry.
  - Permission waits ignore revocation.
  - The approver rule for permission asks lives only on the host.
  - **Anyone at Contributor can sign a `HumanAsk`** with any `agentKey` and
    `permission:true`. The only checks are the role floor and the bounds
    (`authorization.rs:228`, `authz/bounds.rs:77-81`). The agent tier is
    checked only for agent signers (`authz/agent.rs:54,111`). So a member, or
    a compromised box, can draw a card that looks like a host's permission
    prompt.
  - **Anyone can close any ask.** The fold compares the record's `turnId`
    and `agentKey` fields (`flow/mod.rs:714`), not who signed it.
  - **The ACP option fallback can grant a standing permission.** `option_for`
    picks `allow_always` when the agent offers no `allow_once` option
    (`acp.rs:286-296`).
  - The permission question text is the agent's own `title` and `kind`
    (`acp.rs:666-668`).

  **Three of these are fixed in 0.3.1**, ahead of this note (decision D7):
  the `allow_always` fallback, `HumanAsk` spoofing, and closing any ask.
  Sections 5.1 and 6.2 are the 0.4 design for the same rules. They must
  agree with the 0.3.1 fix as shipped, and where they differ, the shipped
  fix is the base this note builds on.

## 3. Goals and non-goals

Goals:

1. Two MCP tools, `ligus_ask_person` and `ligus_ask_room`, which post one
   small decision card. The card has a question, 2 to 5 options or yes/no, an
   optional free-text answer, and who may answer: the person who asked for
   the turn, any member at or above a role, or a quorum. The tools block the
   turn until it is settled or times out, and return the answer.
2. **Answers are signed events** (who, when), and every peer from 0.4 on
   decides the same answer.
3. **One tap** where the answer authorises nothing. Where an answer
   authorises code to run, an apply or a secret, the existing passkey and
   presence rules apply (section 6).
4. The same card on the desktop, lt, the phone and the CLI. Questions from
   sub-agents and child chats appear in the parent chat.
5. `ligus hook permission`: Claude Code's permission prompts from sessions
   Ligus didn't start reach the person's devices as the same card.

Non-goals:

- **Replacing proposals.** A question has no diff, no apply and no vote
  (section 9).
- Letting an agent attach an effect to its own question. Effects are
  written only by the host, and the authz rules enforce that (5.1).
- Resuming a wait across a host restart. The turn is cancelled, as today.
- Rich content in cards: no Markdown links, no images, no attachments.

## 4. The tools

### 4.1 Schemas (new; the existing tool is kept)

`ligus_ask_person`: one person answers.

| Arg | Type | Rule |
|---|---|---|
| `question` | string | required, 1–500 characters after cleaning (6.4) |
| `options` | string[] | 2–5, each 1–60 characters, distinct after cleaning; or omit with `yesNo: true` |
| `yesNo` | bool | options become exactly `["Yes","No"]` (Ligus's words, not the agent's) |
| `allowText` | bool | a free-text answer is allowed, up to 1000 characters |
| `to` | `"requester"` (default) or a member id | `requester` is the human who started the turn; for an agent chain, the person at its root (`author_gate`'s rule, `author_gate.rs:1-26`) |
| `timeoutSecs` | int | 10–3600, default 600 |

`ligus_ask_room`: the room answers.

| Arg | Type | Rule |
|---|---|---|
| `question`, `options`, `yesNo`, `allowText`, `timeoutSecs` | as above | |
| `minRole` | `contributor` (default), `admin`, `owner` | who may answer; never below Contributor, matching `FlowRecorded`'s floor |
| `quorum` | int, default 1 | distinct eligible people who must pick **the same option**; 1–5, and no more than the eligible people at post time (otherwise refused with `quorum_unreachable`) |

Folded selector: `ligus_ask` gains `to: "person"|"room"` (default `room`).
The fold stays within `MCP_RESERVED_SLOTS`. `ligus_ask_human` stays as it is:
it is `ligus_ask_room` with `minRole: contributor, quorum: 1`, 6 options, and
the old length bounds. Its description gains "Prefer ligus_ask_person or
ligus_ask_room." The golden schema hash changes, with a release note.

Budget: all three share the existing asks budget (3 per turn, 6 per cascade).
A call past the budget returns `{"error":"ask_budget"}` without posting.

### 4.2 Results

```json
{"status": "answered", "optionIndex": 0, "option": "Postgres", "text": "keep the dev db",
 "answeredBy": [{"memberId": "…", "name": "Ana", "at": "2026-11-24T10:02:11Z"}]}
{"status": "no_answer", "tally": [1, 0]}                       // timeout; tally only for quorum
{"status": "declined"}                                         // a person closed it ("Not now")
{"status": "cancelled"}                                        // the turn was revoked or stopped
```

`is_error` is set for everything except `answered`. The agent guidance
(`prompt.rs` `ligus_tools_guidance`) says to treat any status other than
`answered` as "no, and don't ask again this turn".

## 5. Records (additive only)

**No new record type.** A new `FlowRecord` variant folds as `Unknown` on older
builds, which treat it as governance and deny it (`flow/mod.rs:372-377`). The
two existing records gain optional fields (`#[serde(default)]`, skipped when
empty), the pattern `permission` already uses.

`HumanAsk`, new optional fields:

| Field | Meaning |
|---|---|
| `v` | 2 |
| `audience` | `{kind: "people", memberIds: [...]}` (one or more named members), or `{kind: "room", minRole}`. Absent means today's rule: any Contributor or above. `to: "requester"` is resolved to a member id **at post time** |
| `quorum` | u8, default 1 |
| `allowText` | bool |
| `effect` | absent (a question), or `{kind: "permission", tool, title, detail, cwd, hostDeviceId, presence: "required"\|"none"}`, or `{kind: "secret", name, hostDeviceId, presence: "required"}`. See section 6 |
| `origin` | `{via: "subagent", label}`, `{via: "chat", chatId}` (a child chat or stage), or `{via: "outside", runtime, label}` (the hook) |
| `settledBy` | on a closing record by the asker: the event id of the answer it acted on, or of the last answer that made a quorum (5.3) |

`HumanAnswered`, new optional fields:

| Field | Meaning |
|---|---|
| `text` | free text, separate from the chosen option's text. `answer` keeps the option's text for older readers |
| `askDigest` | `SHA256("ligus-ask-digest-v1\0" ‖ askId ‖ the ask record's payload bytes exactly as signed)`, computed by the answering device from the **signed envelope it received**, never from a DTO (6.3) |
| `decline` | true for "Not now". It closes the ask for a `people` audience of one; in a room it is that person's abstention |

### 5.1 Which asks count (fold-local, no new refusal)

These rules are applied **in the enveloped fold (5.2), not as authz
refusals**. Every record that 0.3 accepts, 0.4 accepts too. Nothing new is
quarantined, held, alerted or refused, and no device's chain breaks. The
`optionIndex` ingest check (`bounds.rs:85`) sees the same ask map on both
versions. A record that breaks a rule is kept and **folded as inert**: an
effect that doesn't qualify is dropped, and the ask is drawn as a plain
question; a close that doesn't qualify is ignored. A rules-version bump
isn't needed (`RULES_SUPPORTED = 2`, `authz/mod.rs:67`).

- **A question:** any `HumanAsk` without `effect` folds as today.
- **A host's effect counts** when the envelope's signer is the device in
  `effect.hostDeviceId`, **or an agent key certified by that device**
  (`authz/mod.rs:1081`; `post_ask_as` signs with the agent's key, and with
  the host key as a fallback, `chat_service.rs:345-353`). That host must also
  hold the chat's turn claim for `turnId`. An agent key certified by some
  other device, or an effect for a turn its host doesn't hold, is inert.
  Because the agent tool path never writes `effect`, a prompt-injected agent
  can only make an effect by reaching the host's own code, the same trust as
  today's `permission:true`.
- **An outside prompt's effect counts** (8.2) when the signer is a device of
  the person, the actor is that person, `origin.via = outside`, and
  `audience` names exactly that person. It has no turn. A person can raise a
  grant card only to themselves.
- **A legacy `permission:true`** is drawn as a grant only when it would pass
  the host rule above. Otherwise it is a question.
- **Closing.** A `HumanAsk{closed:true}` (or with `settledBy`) counts only
  from the ask's first signer, or from the other half of the same pair (the
  host device or an agent key it certified). This applies to every ask, v1
  included. A 0.3 peer still folds a forged close as closed, which is
  display only.
- **Readers** draw grant chrome (the effect line, Allow once, Deny) only for
  an ask whose effect counts on their own fold.

### 5.2 The fold

Asks move to an **enveloped fold**, the way memory does
(`memory::apply_enveloped`, `memory.rs:880-897`):

- **Inputs.** The fold sees the signer's account, device and actor kind, and
  the stamp's config frontier (`ActorStamp.cfg_frontier`). `FlowState::apply`
  alone sees none of these (`events.rs:1190`).
- **Determinism.** The result is a function of the chat log in canonical
  order plus the config log up to each stamp's frontier, so every peer with
  the same frontiers decides the same. **No wall clock enters the fold.**

For `v: 2` asks:

- **Eligibility.** An answer counts only if it is from a human eligible under
  `audience`, judged by the roster **at the answer's stamped config
  frontier**: a named member, or a member whose role there is at least
  `minRole`. That frontier may be **no older than the ask's own frontier**
  (the existing "no config position older than the content" rule,
  `authz/mod.rs:736-743`, applied here in the fold). A demoted member can't
  back-date one to stay eligible. The answer must also match `askDigest`.
  Ineligible answers are kept but don't count.
- **Quorum.** Walking the chat log in canonical order, the latest answer per
  eligible account counts. The ask settles at the **first position** where
  one option has `quorum` distinct accounts, and then it is frozen. A later,
  or later-merged, answer never un-settles it. With `quorum: 1` this is "the
  first eligible answer wins".
- **The asker has the last word.** When the host acts, it writes a closing
  `HumanAsk` with `settledBy`, and readers show that. If a back-dated answer
  merged later would sort earlier, every peer still converges on what the
  host acted on.
- **Expiry is a view, not fold state.** Readers and attention counts hide an
  ask whose `expiresAtMs` has passed. The host ignores answers after it
  stopped waiting.
- **Eviction uses a v1 shadow.** The 256-ask eviction (`flow/mod.rs:717`)
  feeds the ingest check of `optionIndex` (`bounds.rs:85`), so it must pick
  the same asks on 0.3 and 0.4. Each ask therefore keeps a **v1 shadow**: the
  raw first answer and the raw `closed` flag, folded exactly as 0.3 does,
  forged closes and ineligible answers included. Eviction and that ingest
  check read only the shadow. The v2 state (eligibility, quorum, the closing
  rule, `settledBy`) drives display and the host alone, so nothing a peer
  accepts or refuses depends on it.
- An ask without `v` folds as today, except that the closing rule above now
  applies.

### 5.3 Restart sweep

When a host starts, it writes `HumanAsk{closed:true}` for every open ask
whose turn it owned. Its pending turns are cancelled anyway
(`docs-site/docs/ligus/addons/ligus-mcp.md`, "Questions to the chat").

## 6. What an answer authorises

### 6.1 Two kinds of card

- **A question** (no `effect`): the agent's tools post these. The answer is
  text the agent reads. It grants the agent no new permission: it can't
  approve a proposal, vote, apply, run something above the agent's tier or
  reveal a secret. The agent may act on it **within what it could already
  do**: delete fixtures it was already allowed to edit, for example. Anything
  beyond that still goes through its tier, the sandbox, proposals and apply
  rules. The answer needs no passkey; on the phone see 7.4 and D1.
- **A grant** (`effect` present): only the host posts these (5.1). The answer
  lets the host do something it otherwise wouldn't:
  - `permission`: an ACP escalation, or a hook prompt (section 8). Allow
    once, for exactly the tool call shown.
  - `secret`: reserved for a later secret-access flow. In 0.4 no host posts
    one, but the shape and its rule are fixed now.

  An "apply" effect is **not** a card. When an agent wants a proposal
  applied, the card's single action opens the existing Review screen
  ("Approve & apply", `docs/design/review-screen-approve-and-apply.md`), and
  that screen's passkey rules apply. A card never applies anything.

### 6.2 Rules for grants

**Who may grant.** Only the members in `audience`:

- for ACP, `{people: [host's person, agent's owner]}`, which is today's
  `answered_by_approver` (`ligus_mcp/server.rs:1435-1446`), now also in the
  fold;
- for the hook, the person alone.

**When a passkey is needed.** The host decides and **writes it in the
record**, as `effect.presence`. Peers can't evaluate "human-gated" or
"outside the sandbox" themselves (`human_gated` is defined over a proposal's
paths, `rules.rs:187-194`). The host sets `required` when any of these hold:

- the chat is in lockdown;
- dearth mode is on;
- the call runs outside the workspace sandbox;
- the effect is `secret`;
- the prompt came through the hook (8.2), because Ligus didn't start that
  agent and its sandbox is unknown.

**The rule (fold-local, like 5.1).** An answer to an ask whose
`effect.presence` is `required` **counts only if it carries the person's
passkey presence** in its stamp. It isn't refused without it. It is kept and
doesn't count, so `authz/rules.rs` is unchanged and versions agree on
acceptance. The host checks the same before it acts.
Where presence is `none`, a desktop or lt grant is one click after a
2-second hold; a phone grant always asks Face ID.

**Allow once, really once.**

- The ACP host answers with the agent's `allow_once` option only. **The
  fallback to `allow_always` (`acp.rs:286-296`) is removed:** with no
  `allow_once` option on offer, the host answers `reject_once` and the card
  says "This agent offered only a standing permission, which Ligus never
  gives."
- The hook never prints `updatedPermissions` or `updatedInput`.

### 6.3 Why `askDigest`, and what it covers

The attack: a phone answers through a `ligus serve` box, and that box (a
service member, possibly compromised) builds the phone's view (`ask_dto`,
`serve/api.rs:419-433`) and the preimage it signs. A lying box could:

- show a permission ask as a question, so the phone skips Face ID;
- swap the options, so "Deny" is index 0 (the host grants on
  `optionIndex == 0`, `acp.rs:673`).

The fix:

- The box passes the ask's **raw signed envelope**.
- The phone checks the envelope signature, parses the payload bytes itself,
  and draws the card from them. Grant buttons come from Ligus constants by
  index, never from text.
- The phone signs `askDigest` over those exact payload bytes, and also checks
  that the box-drafted `HumanAnswered` preimage names that `askId`, that
  digest and the `optionIndex` the person tapped.
- The host and the fold count an answer only when `askDigest` equals the
  digest of the record the host posted. Any change to the question, the
  options or the effect makes the answer count for nothing.
- No canonical JSON is involved: the bytes are the ones the envelope signs
  (`envelope_wire.rs` keeps them verbatim). Golden vectors are shared with
  the phone repositories.

### 6.4 Sanitising agent-written text

Agent text is untrusted. It may come from a prompt-injected turn.

- **Plain text, not Markdown**, for `v: 2` cards. No links are rendered: a URL
  shows as text, can be selected and copied, and is never clickable or opened
  automatically. Older `ligus_ask_human` asks keep today's Markdown renderer
  (click-only links) until 0.4, when they switch to plain text too (D4).
- **Cleaning**: `clean_display_name`-style removal of bidi controls,
  zero-width characters and other controls, applied to the question and the
  options.
  - Options are one line: newlines and tabs become a space, and runs of
    spaces collapse.
  - The question keeps at most 8 line breaks.
  - The cleaning happens on the host before posting, and again on every
    reader, so a crafted record can't bypass it.
- **No look-alike chrome**: an option may not equal (case-insensitively, after
  cleaning) a Ligus control label ("Allow once", "Approve", "Approve & apply",
  "Deny", "Not now"). The host refuses such an ask with `option_reserved`.
- **What the host can't verify is labelled.** An ACP permission card shows
  the agent's `title` as "Builder describes it as: …". Below it goes the
  tool call's own content (the raw input, the command or the locations, when
  ACP provides them) as the detail, since those are what the allow-once
  covers.
- **Sizes**: question 500 characters, options 60, free-text answer 1000. The
  record bounds stay as they are (2000/200/2000), so old asks still fold.

### 6.5 Rate and volume

- The per-turn and per-cascade ask budgets are unchanged.
- **Per agent per chat:** at most 2 open asks. A third returns
  `{"error":"too_many_open"}`.
- **Per chat:** at most 5 open asks. A sixth is refused for every agent.
- **Per person, across chats:** at most 10 pushes an hour. After that, one
  "Several agents are asking" Inbox row, and no more pushes until it is
  opened.
- **Push:** one per ask, collapsed per chat. A person is pushed only when
  they are in the ask's audience (the phone's "next actor" badge rule,
  `docs/android-app.md:1273-1305`).

## 7. UX

### 7.1 The card (every surface)

One `Card` per open ask: a single object that needs attention, as the calm-UI
rules allow (`web/README.md`, "Copy in panels").

```
 ┌──────────────────────────────────────────────────────────────┐
 │ ◐ Builder asks you                         (Ana's mbp · 9:41)│   ← Ligus chrome
 │ "Use Postgres or SQLite for the test fixtures?"              │   ← agent text, quoted
 │  [ Postgres ]  [ SQLite ]                     Reply…  Not now│
 │ Builder decides what to do next. No new permission.          │   ← Ligus chrome, muted
 └──────────────────────────────────────────────────────────────┘
```

- **Header (Ligus's words).** The agent's avatar and name, "asks you" (for a
  person audience) or "asks Admins" or "asks the room" (with the quorum, if
  any: "needs 2 of you"). The host device and the time go behind the muted
  meta. `origin` adds a tag: "via sub-agent", "from <child chat>", or
  "outside Ligus".
- **Body (the agent's words)**, in quotes, plain text, in the body font,
  never styled like a control.
- **Options** are buttons in order. Yes/no shows **Yes** and **No**. Free
  text is a "Reply…" link that opens a one-line field. **Not now** is a muted
  text button.
- **The effect line, in Ligus's words, always shown and never hidden**
  ("Never hide what protects someone"):
  - Question: "Builder decides what to do next. Your answer grants no new
    permission."
  - Permission: "Allow once lets Builder run this once on Ana's mbp:",
    followed by the tool call's `detail` in mono (the full command and its
    working directory, or the file and its content or diff). The detail
    scrolls, and is never cut without an ellipsis that expands. Then
    **Allow once** and **Deny**, in Ligus's labels. The agent's own `title`
    is shown as "Builder describes it as: …" (6.4).
- **Not eligible**: the card shows read-only for others. "Waiting on Ana" or
  "Waiting on Admins · 1 of 2" replaces the buttons.
- **Settled**: the card collapses into one line in the transcript: "Ana
  answered Postgres · 9:42". The answer is a signed event, and the line's
  tooltip names the device that signed it.
- **Expired**: "Builder stopped waiting." The buttons go.

### 7.2 Desktop

- The card sits in the transcript where the agent asked. The **Review pane**
  keeps its list of open asks (`AgentAsks`). The **Inbox** has one row per ask
  where you can answer:
  - "Builder asks you", with the chat name muted.
  - Opening the row opens the card in its chat.
  - **This is a change:** today the Inbox embeds `AgentAsks` with its answer
    buttons (`Inbox.tsx:44-56`). 0.4 removes them, so consequences are seen
    in context first, as the Inbox rule says (`Inbox.tsx:18-20`).
- The Inbox shows asks you can't answer only when nothing else is waiting.
- Keyboard: number keys 1–5 pick an option while the card has focus, and
  Enter confirms. A grant needs the button itself or the hold.
- `AskDto` gains `kind`, `audience`, `eligible`, `effect`, `origin` and
  `tally`, so a permission ask is drawn as a grant, fixing today's gap.

### 7.3 lt

The card is an inline block in the transcript:

```
Builder asks you: "Use Postgres or SQLite for the test fixtures?"
  1 Postgres   2 SQLite   r reply   n not now
  Builder decides what to do next. No new permission.
```

- Keys 1–5 pick, `r` opens a one-line reply, `n` says Not now.
- A grant shows the exact detail, and **Allow once** needs `a` and then a
  confirm. Where a passkey is needed, it uses the browser passkey (lt is a
  delegated device, as for votes).

### 7.4 Phone

- **Push**: "Builder asks you · <chat>". The text is cut to 80 characters,
  with no options in the notification. It opens the card. iOS shows no
  answer actions on the push, so nothing is decided from the lock screen.
- **The card** is drawn from the raw signed ask (6.3). It has the same order
  and the same effect line, and options are full width.
- **Face ID.** In 0.4 every phone answer still asks Face ID, as today. One
  tap for questions comes later (D1): a 0.3 host checks no `askDigest`, so
  while such hosts exist a lying box could pass off a permission ask as a
  question.
- A grant uses Face ID with reason "Allow Builder to run cargo publish
  --dry-run on Ana's mbp" (the detail is cut to 60 characters in the reason,
  and the full detail is on screen).
- `ask_dto` gains the same fields as the desktop plus the raw signed
  envelope. The phone computes `askDigest` itself (6.3).

### 7.5 CLI (a person's own CLI device)

```sh
ligus asks list [--chat ID] [--mine] [--json]                 # open asks, eligibility, effect
ligus asks answer <ask-id> (--option N | --yes | --no) [--text "…"] [--json]
ligus asks decline <ask-id> [--json]
```

- **Any answer asks for confirmation on the terminal** (`/dev/tty`, "Answer
  Builder's question with 'Postgres'? [y/N]"), and exits 4 without one. An
  agent running as the person (a Claude Code session outside Ligus has no
  Ligus sandbox) can't settle questions or quorums this way. It is a human
  step, like `ligus admit issue`.
- **A grant always needs a passkey** through the browser hand-off, whatever
  `effect.presence` says. There is no one-click path from the CLI.
- **Asks from the hook (`origin.via = outside`) can't be answered from the
  CLI at all.** The CLI is the very place the asking agent could reach.
- Other exit codes: 5 for an unknown ask; 6 for not eligible, or a refusal by
  the workspace rules; 1 for an ask that is already settled or expired.
- `--json` prints one `ligus.agent-deploy.v1` document.

## 8. Sub-agents and outside agents

### 8.1 Sub-agents and child chats

- **`Task`/`Agent` sub-agents of a Claude Code turn** share the turn's MCP
  credential (`claude_code.rs:227-231`), so their asks already post in the
  parent chat as the parent agent.
  - The host can't name the sub-agent through MCP, so `origin` is
    `{via: "subagent"}` only when the turn's runtime reports one (ACP's tool
    call metadata).
  - Otherwise the card shows the parent agent, which is the truth: the
    parent is responsible.
- **ACP permission requests from a sub-agent** arrive on the same session
  and become the same grant card.
- **Child chats** (a handoff, a workflow stage) keep the ask on the child
  chat's log, where it is signed and answered. The parent chat shows a
  pointer card, "From <child chat>", rendering the same ask with the same
  buttons. The answer is written on the child's log.
  - The pointer is computed by readers from the existing handoff and
    workflow records; nothing new is logged.
  - One ask still sends one push.

### 8.2 `ligus hook permission` (Claude Code sessions Ligus didn't start)

This uses Claude Code's `PermissionRequest` hook (Claude Code hooks reference,
code.claude.com/docs/en/hooks, read 2026-11-24; to be re-checked against the
live page and a local test before building). As documented there:

- **When it runs.** The hook runs when Claude Code is about to show a
  permission dialog, after `PreToolUse`.
- **Input.** It gets JSON on stdin: `tool_name`, `tool_input`, `cwd`,
  `session_id`, `permission_mode`, and optionally `permission_suggestions`
  and `mcp_server`.
- **Output.** It decides by printing
  `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"|"deny","message":"…"}}}`
  and exiting 0.
- **No decision.** Exit code 2 is not a decision on this event. Printing
  nothing, failing or timing out leaves the normal flow: the dialog in an
  interactive session, and a **deny in `-p` mode**.
- **Default timeout:** 600 s.
- **Unverified:** whether the hook fires for sub-agent prompts, and with
  which `agent_id`. It needs a local test; until then the card's `origin`
  names no sub-agent.

```json
{"hooks": {"PermissionRequest": [{"matcher": "*", "hooks": [
  {"type": "command", "command": "ligus hook permission --workspace W --chat C", "timeout": 120}]}]}}
```

What `ligus hook permission` does:

1. **Connects to a running Ligus process** of this user over a local
   socket: the desktop, lt or `ligus serve`, in that preference.
   - The socket is owner-only (`0600`) and lives in the process's data dir.
     The client presents a per-install hook token from `ligus hook token`,
     also stored `0600`.
   - The hook **never opens a data dir itself** (AGENTS.md rule 4).
   - With no running process it prints nothing and exits 0. That is Claude
     Code's own dialog, or a deny in `-p`. The hook never auto-allows.
   - A same-user process can read the token, so anything running as the
     person can raise prompts to the person (risk A8). It can't answer them,
     because hook asks are never answerable from the CLI (7.5).
2. **Posts the grant card.**
   - Who signs: the record is signed by that process's device as the
     person, which 5.1 allows only with `origin.via = outside` and
     `audience = {people: [that person]}`. A person can raise a prompt only
     to themselves. The same device closes it.
   - The effect: `effect = {kind: "permission", tool: tool_name, detail,
     cwd, hostDeviceId, presence: "required"}`. Ligus didn't start this
     agent and doesn't know its sandbox, so a grant always needs a passkey
     on the desktop and lt, and Face ID on the phone.
   - What `detail` holds: for `Bash`, the exact command. For `Write`, `Edit`
     and `NotebookEdit`, the path plus the full new content or the diff.
     Anything larger than 8 KiB gets **no Allow at all**: the card says
     "Too large to review here. Answer at the terminal." and offers only
     Deny. Nobody approves a write they can't see.
   - Other tools: `tool_input` as pretty JSON, under the same cap.
   - `permission_suggestions` and `mcp_server` are shown under Details and
     never applied.
   - `origin = {via: "outside", runtime: "claude-code", label: <basename of
     cwd>}`. The card's header says "Claude Code (outside Ligus) asks to
     run…" and carries the "outside Ligus" tag: Ligus didn't start this
     agent and knows only what the hook says.
3. **Waits** until an answer, `--timeout` (default 90 s, and always 10 s
   under the settings' `timeout`), or the hook being killed. A person at the
   terminal is blocked while it waits, which is why the default is short.
   Then it prints:
   - **Allow once:** `behavior: allow`, with no `updatedPermissions` and no
     `updatedInput`. Ligus never adds a standing rule.
   - **Deny:** `behavior: deny` with "Denied from Ligus by <name>", and
     `interrupt: false`.
   - **Timeout or no answer:** nothing. Claude Code then shows its own
     dialog, or denies in `-p` mode. The host writes the ask closed.

**Where the card lives.** Every chat is visible to every member of its
workspace (there are no private chats), and a hook prompt can show commands,
paths and file content. So `--workspace` and `--chat` are required, and
`ligus hook install` asks for them. The target is a workspace of one (decision D3), which the desktop offers to make as "Personal". The hook prints a warning to stderr, once per chat,
if the workspace has other members.

**Limits.** 10 hook asks per minute per install, and at most 3 open. Past
that, the hook prints nothing.

`ligus hook install [--settings PATH]` writes the hook entry into a Claude
Code settings file the person names. It is idempotent and prints the diff.
It is never run for the person, and it is human-only for user-level settings.
`ligus hook --help` documents it all (self-documenting CLIs).

## 9. How this differs from proposals

| | Agent question | Proposal |
|---|---|---|
| Content | a question and 2–5 short options | a diff, a command or a decision, with a digest |
| Effect | none (a question), or one host-written, allow-once grant | apply, run, decide; changes the workspace |
| Vote | no; a quorum only when the agent asks for one, and it's a tally, not approval | approvals at a role floor, author excluded, rejects block |
| Passkey | a question never; a grant by the rules in 6.2 | by the proposal and apply rules |
| Life | minutes (10 s to 1 h); ends with the turn | until decided or dismissed |
| Where | the card in the chat, the Inbox row, the push | the Review pane and the Review screen |

An agent that needs a change approved files a proposal. An agent that needs a
preference asks a question. The guidance text says so.

## 10. Prompt injection and other risks

| # | Risk | Control | Residual |
|---|---|---|---|
| A1 | A **misleading question** from an injected agent ("Yes is safe, it only cleans caches") | The question is quoted and attributed to the agent. The effect line is Ligus's and says the answer grants no new permission. Whatever the agent does next stays within its tier and its gates | The agent may act on the answer within what it could already do; a bad proposal still meets its review |
| A2 | An agent **posing as Ligus**: options named like controls, fake effect text in the question, bidi tricks | Reserved option labels refused; cleaning; plain text; the effect line drawn outside the quoted area in Ligus's style | Wording inside the quote can still lie; it is visibly the agent's |
| A3 | **Forged grant cards**: a member, a box or an agent signs a `HumanAsk` with an `effect` or `permission` | 5.1: an effect counts only from the turn's host device or an agent key it certified, or from a person to themselves from outside; readers draw grant chrome only for asks whose effect counts | A 0.3 reader may draw a forged `permission:true` ask as it does today; 0.4 hosts never act on it |
| A4 | **Forged closes** to silence an ask | 5.1: only the ask's own signer closes it | 0.3 peers still accept forged closes for display |
| A5 | **A grant's detail differs from what runs** | The detail is the tool call's own content; allow-once only; the `allow_always` fallback removed; writes too large to show get no Allow | The runtime must honour allow-once; an ACP agent's `title` is unverified and labelled so |
| A6 | **A box or relay changes what the phone sees**: effect, question or option order | The raw signed envelope goes to the phone; `askDigest` over its payload bytes; grant labels by index from constants; the preimage is checked (6.3) | Withholding or delay (DoS) |
| A7 | **Answer by the wrong person** (a teammate taps Allow on your permission prompt) | `audience` in the fold, at the answer's config frontier; the host check stays | Older peers may display such an answer as settled (section 11) |
| A8 | **An agent answering as the person** through their CLI, or abusing the hook socket | **Presence is the real control**: every grant, and every hook ask, needs the person's passkey or Face ID. The CLI's terminal confirmation and its refusal of hook asks only stop casual misuse. The socket is owner-only with a token, rate-limited, and can only raise prompts to the person | An agent running as the person outside Ligus can read that install's device key and answer **questions and quorums** as the person. It can't grant. Prompts it raises are labelled "outside Ligus" |
| A9 | **Ask flooding** to wear people down | Budgets, open-ask caps, a per-person push cap across chats, push collapse, no lock-screen actions | None |
| A10 | **Links** in a question used for phishing | Not rendered as links on new cards; never opened automatically on any card | A person can still copy and paste a URL |
| A11 | **Hook data leaked to teammates** | Explicit `--workspace`/`--chat`; a warning when others can read it; the Personal workspace of one (D3) | A person who picks a shared chat shares their prompts |
| A12 | **Stale answers** after a restart or timeout | The restart sweep, expiry as a read-time view, the host ignoring answers after it stopped waiting, `settledBy` | None |

**Offline.** An answering device that is offline signs locally and syncs
later. If the answer arrives after the host stopped waiting, the card says
"Too late: Builder stopped waiting", and nothing happens. A host that is
offline from the relay doesn't see answers, so its turn times out with
`no_answer`. The hook then prints nothing: Claude Code's own dialog, or a
deny in `-p` mode.

## 11. Mixed versions

| Peer | Sees a `v: 2` ask as | Effect |
|---|---|---|
| 0.3 desktop or phone | a question with options (the new fields are ignored), free text always allowed | Its answers carry no `askDigest`, so **0.4 hosts and folds don't count them** for a `v: 2` ask. In practice, 0.3 members can't answer new asks until they update; the release note says so. The 0.3 peer may show the ask as settled when it isn't. The 0.4 card shows "Answered on an older version; not counted" |
| 0.3 host | | Never posts `v: 2` asks. Its asks fold as today on 0.4, apart from the closing rule (5.1) |
| 0.3 reader of a forged `permission:true` ask | a question, as today | No host acts on it (a host acts only on its own asks) |
| 0.4 phone through a 0.3 box | the old `ask_dto` with no raw envelope | The phone treats every ask as a possible grant and asks Face ID (today's behaviour) |

Nothing is renamed. `ligus_ask_human` keeps working. New fields are optional
and pinned by golden JSON tests. **No new authz refusal**: the rules in 5.1
and 6.2 are fold-local, so 0.3 and 0.4 accept and refuse exactly the same
events. They can differ only in how an ask is drawn and settled, and the
host's own fold decides what runs.

## 12. Tests (executable, required before merge)

| # | What | Where |
|---|---|---|
| Q-T1 | Schemas: bounds (500/60/1000), 2–5 options, yes/no labels fixed, reserved option labels refused, `quorum_unreachable`; golden hash updated | `ligus_mcp::tools` tests |
| Q-T2 | Fold-local rules (5.1): an effect signed by the host device, or by an agent key it certified, with the turn claim, counts. An effect from a member, a service, another host's agent key or a host without the claim is inert (the record is accepted, and the ask is drawn as a question). A person's effect counts only with `outside` origin and themselves as audience. A close from anyone but the ask's signer pair is ignored. **The acceptance verdicts of 0.3 and 0.4 are identical** over a corpus of such records: no quarantine, hold, alert or chain break | `flow` and `authz` tests |
| Q-T3b | Eviction shadow: under forged closes, ineligible answers and quorum asks past the 256 cap, 0.3 and 0.4 folds keep the same set of ask ids and give the same `optionIndex` ingest verdicts (property test) | `flow` tests |
| Q-T3 | Enveloped fold: person audience ignores others; `minRole` judged at the answer's config frontier (a demoted member's later answer doesn't count, and a back-dated frontier older than the ask's doesn't count either); quorum by latest answer per account, frozen at the first canonical position; `settledBy` wins; no wall clock (the same log folds the same at any time); eviction unchanged | `flow` tests, property tests over orderings and merges |
| Q-T4 | `askDigest`: changing the question, the option order or the effect makes the answer count for nothing on the host and in the fold; the phone digests the raw payload bytes and draws grant labels by index; golden vectors shared with the phone repositories | `flow`, `serve`, phone tests |
| Q-T5 | Grants: ACP answers only `allow_once`, and with none on offer answers `reject_once` (regression test for the removed fallback); `effect.presence: required` enforced in the fold and on the host; a teammate's Allow is a deny; a revoked turn stops the permission wait | `acp`, `ligus_mcp::server` tests |
| Q-T6 | Restart sweep closes every open ask the host owned; late answers say "too late" and do nothing | `agent_turn` tests |
| Q-T7 | Sanitising: bidi, zero-width, controls removed; options one line; URLs not linked in `v: 2` cards; the effect line is outside the quoted area (DOM test) | `web` Vitest, `display` tests |
| Q-T8 | Sub-agents and child chats: a child-chat ask shows in the parent, an answer from the parent lands on the child's log, one push | `workflows` tests, `web` |
| Q-T9 | Hook: allow, deny, timeout and no running process print nothing; a `-p` session sees a deny then; writes over 8 KiB offer no Allow; never `updatedPermissions` or `updatedInput`; passkey always required; the socket refuses another user and a missing token; rate limit; output matches the documented shape (golden); a local test records whether sub-agent prompts fire the hook | `ligus-cli` hook tests |
| Q-T10 | CLI: `asks answer` without a terminal exits 4; a grant needs a passkey; hook asks refused from the CLI; exit codes and `--json` | `ligus-cli` |
| Q-T11 | Mixed versions: a 0.3 peer's answer to a `v: 2` ask is not counted on 0.4; `ligus_ask_human` behaves as before | `flow` tests with recorded 0.3 envelopes |
| Q-T12 | Calm UI: design-QA with an open card and an Inbox ask row (one banner at most, badge limits, AA contrast); no answer buttons on Inbox rows; screen readers read header, quote, options and the effect line in that order | design-qa, `web` a11y tests |

## 13. Decisions (owner, 2026-11-25)

The owner accepted every recommendation.

| # | Question | Decision |
|---|---|---|
| D1 | When may phone answers to questions skip Face ID? | **Not in 0.4.** From the release where hosts below 0.4 can no longer be in a workspace, a phone may skip it for an ask it verified from the raw envelope as `v: 2` with no effect. Grants keep Face ID always |
| D2 | Desktop passkey for permission grants? | **Only when `effect.presence` is `required`** (lockdown, dearth mode, outside the sandbox, every hook prompt); otherwise one click after the 2-second hold |
| D3 | Where hook prompts go | **A workspace of one**, which the desktop offers to make ("Personal"); never a shared chat by default. A device-local route to your own devices only is a later option |
| D4 | Plain text for old `ligus_ask_human` cards? | **Yes, in 0.4**, one renderer for every card |
| D5 | Define `secret` effects now? | **Define the shape and the rule, post none**, so 0.5 adds no wire change |
| D6 | Does "Not now" on a room ask close it for everyone? | **No**, only for a single named person; in a room it is that person's abstention |
| D7 | The ACP `allow_always` fallback | **Fixed separately in 0.3.1**, together with `HumanAsk` spoofing and closing any ask (section 2) |

## 14. Review

One adversarial reviewer, read-only, two passes against the code at
`3c1638ddf`.

**First pass: Revise.** Most of the security model had no authz backing in
the code, or contradicted it.

| # | Finding | Severity | Resolution |
|---|---|---|---|
| R1 | Grant cards could be forged: any Contributor (or a box) can sign a `HumanAsk` with any `agentKey` and `permission:true` (`authz/agent.rs:54,111`, `authorization.rs:228`) | Must-fix | 5.1: an effect counts only from the turn's host device or an agent key it certified (with the turn claim), or from a person to themselves from outside; readers draw grant chrome only then |
| R2 | Anyone could close any ask (`flow/mod.rs:714` compares fields, not the signer) | Must-fix | 5.1 closing rule, for every ask |
| R3 | The fold changes weren't deterministic (no envelope in `FlowState::apply`; expiry by wall clock fed eviction and ingest) | Must-fix | 5.2: an enveloped fold at stamped config frontiers, no wall clock, expiry as a view, eviction unchanged, quorum frozen at first settle, `settledBy` |
| R4 | `effectDigest` didn't stop a lying box swapping options; canonical JSON from a DTO | Must-fix | `askDigest` over the raw signed payload bytes; the phone draws from the raw envelope; grant labels by index; preimage checked (6.3) |
| R5 | Dropping Face ID while 0.3 hosts exist | Must-fix | Face ID kept in 0.4; one tap only once no host is below 0.4 (D1) |
| R6 | "Allow once" false in code: `option_for` falls back to `allow_always` (`acp.rs:286-296`); the detail is agent-supplied | Must-fix | Fallback removed (D7: fixed in 0.3.1); the agent's title labelled as its own; the detail is the tool call's content (6.2, 6.4) |
| R7 | An agent could answer its own cards through the person's CLI | Must-fix | Grants and hook asks always need presence; CLI answers need a terminal; hook asks not answerable from the CLI (7.5) |
| R8 | One `memberId` couldn't say "host's person or agent's owner" | Should-fix | `audience: people` with several ids |
| R9 | The passkey condition wasn't evaluable by peers | Should-fix | The host writes `effect.presence`; the hook always requires it (6.2) |
| R10 | The hook didn't match Claude Code: `-p` silence denies, the timeout was long, writes were approved blind, the signer was undefined | Should-fix | 8.2 rewritten |
| R11 | "It allows nothing" misled; the Inbox already had answer buttons | Should-fix | New copy; the Inbox change stated (7.1, 7.2) |
| R12 | No per-person push cap; 0.3 members effectively can't answer v2 asks | Nit | Cap of 10 per hour; release note (6.5, 11) |

**Second pass: Approve with risks**, every first-pass Must-fix resolved.
Folded in:

| # | Finding | Severity | Resolution |
|---|---|---|---|
| S1 | The new refusals weren't behind a rules version, so 0.3 and 0.4 would split on acceptance (and, through `bounds.rs:85`, on answers) | Must-fix unless refusals were only dropped | **No new authz refusal.** 5.1 and the 6.2 presence rule are fold-local: records are accepted as today and fold as inert. Q-T2 asserts identical acceptance verdicts across versions |
| S2 | The signing rule didn't match how hosts sign (agent keys certified by the host, `authz/mod.rs:1081`); hook asks have no turn; closes through the host-key fallback | Should-fix | 5.1 restated: the host device or an agent key it certified; a separate outside-prompt case; closes by either half of the pair |
| S3 | A back-dated config frontier kept a demoted member eligible | Should-fix | The answer's frontier may be no older than the ask's (`authz/mod.rs:736-743` rule, in the fold); Q-T3 |
| S4 | A8 overstated the CLI controls | Nit | A8 says presence is the real control; questions and quorums remain answerable by an agent running as the person outside Ligus |

**Third pass.** One Must-fix: eviction read `is_open`, which 0.4 folds
differently (the closing rule, v2 settlement), so the two versions could
evict different asks and split `optionIndex` ingest. **Resolved:** a v1
shadow per ask, folded as 0.3 does, drives eviction and that check only
(5.2). Test Q-T3b. Verdict after the fix: **Approve with risks**.

**Residual risks.**
- An agent running as the person outside Ligus can answer questions and
  quorums as them, but never grant.
- 0.3 peers may draw forged or ineligible states for display.
- A misleading question can steer what an agent does within its own tier.
