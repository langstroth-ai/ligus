# Shared live sessions (design note)

Status: design only, not implemented. Written during 0.2, to be built after
the preview layout. Ligus is short for *ligustica*, the Italian honeybee.

Several people in one chat while an agent works: who is here, who is typing,
who is **steering** the agent, following someone's view, handing the steering
to someone else, and commenting on a running turn without stopping it. All of
it end-to-end encrypted, on a self-hosted relay. This note settles the four
questions that have to be answered before building: what presence leaks, what
it costs the relay, who may steer, and how a hand-over is authorised.

## 1. What exists today (code-verified)

`hive` at `origin/main` `001a9b8a6`; the relay is
`langstroth-ai/langstroth-relay` at `origin/main` (`v0.3.0`). Note:
`docs/langstroth-relay-api.md` still describes a plaintext presence record
(names, device names, session ids); the code no longer does that, and the
API doc should be corrected separately.

### 1.1 Presence

- **Relay.** `POST`/`GET /v1/workspaces/{id}/presence` (`server.go:321-322`):
  opaque slots `{device_id, data}` overwritten in place, keyed by
  `(workspace, device_id)`; at most 1,000 slots per room (`hardening.go:61`);
  **no TTL** (readers judge staleness) and **no push** (writing presence does
  not fire the SSE nudge, only an envelope append does, `memory.go:344`). On a
  room with membership, writes need Contributor (`members.go:156-162`), so a
  Viewer cannot publish presence.
- **What the relay sees:** the room id; slot names, which carry ids
  (`human:<device_id>`, `host:<host_id>`, `lease:workflow_run:<run_id>`;
  `crates/ligus-runtime/src/human_presence.rs:44`, `host_presence.rs:36`,
  `lease_presence.rs:36`); blob sizes and write timing; client IP and token
  subject. The blob itself is sealed.
- **Human beats.** `HumanBeat { actorId, name, deviceId, sessionId, typing,
  ts }` (`human_presence.rs:51-67`), sealed under the workspace seal key,
  **unsigned** (any member can forge another's typing indicator, as its doc
  says, `:22-24`). Readers drop beats older than 8 s (`PRESENCE_TTL_MS`) or
  skewed by more than 30 s. The desktop publishes only while typing
  (`web/src/components/ChatView.tsx:256-275`, at most every 1.5 s, `false` 3 s
  after the last key), gated by the `share_typing` setting; it polls every
  3 s in a chat and every 5 s in the People pane. So "online" today means
  "typed in the last 8 seconds".
- **Phones** go through `ligus serve` (`crates/ligus-cli/src/serve/typing_api.rs`):
  republish every 3 s, poll every 2 s while a phone listens.
- `PresenceSheet.tsx` is the passkey ("presence" as in user presence)
  modal, unrelated.

### 1.2 Turns

- **Who runs a turn:** the responder's owner device, not the sender
  (`turn_runs_here`, `crates/ligus-runtime/src/turn_dispatch.rs:69`; the two
  gates `send_message` and `maybe_respond` in `app/src/lib.rs`). In a room with
  more than one person, an un-addressed message triggers nothing.
- **Turn claims:** `TurnClaimed { trigger_id, device_id, epoch, supersedes,
  repo_id }` (`events.rs:79`), first in canonical order wins; hand-over of a
  turn by `epoch + 1` with `supersedes` (`crates/ligus-core/src/dispatch.rs`),
  signed by the device it names.
- **Who may trigger:** any Contributor+ authoring a message
  (`authorization.rs:229-294`), then the turn gate `author_gate.rs`: the
  lowest human role in the trigger chain must be Admin for a turn that can
  write, Contributor for read-only.
- **Live output:** the runner checkpoints the text as signed
  `MessageChunkReceived` envelopes every 750 ms (`turn_engine/mod.rs:44`),
  redacted; others see it within a second or so. Tool activity, thinking and
  phases are local only (`turn_host.rs:126-128`).
- **Stop:** `stop_turn` (`app/src/lib.rs:9483`) acts on the in-process
  registry. **Only the device running the turn can stop it.** There is no
  synced stop.
- **Messages during a turn** are queued locally and sent when it ends
  (`ChatView.tsx` ~830-840).

### 1.3 Leases

`CoordinatorLease { scope, scope_id, holder: device, epoch, lease_ms,
reason, claimed_at }` and `LeaseRelease { …, successor }`
(`crates/ligus-core/src/lease.rs:53-126`): epochs strictly increase, only the
named successor may take the next epoch, lease 60 s, beats every 15 s,
signed beats (`LEASE_BEAT_CONTEXT`). Events `CoordinatorLeaseClaimed` /
`CoordinatorLeaseReleased`, Contributor floor, signed by the holder.
Leases are **device-keyed**.

### 1.4 Relay budget (public relay profile)

Per-client 20 req/s; **per-room 50 req/s across all clients, every route
including presence** (`scope.go:76-101`); SSE 1,200 connections; envelope
appends 20/s per writer. A streaming turn is about 1.3 appends/s from the
runner plus one pull per watcher per nudge.

## 2. Shape

### 2.1 Presence: three states, opt-in for "viewing"

A beat gains two optional fields (additive, sealed as today):

```
HumanBeat { …existing…, viewing: Option<Uuid /* sessionId */>, v: 2 }
```

- **Typing** stays as today (`share_typing`).
- **Viewing** is published only when the person has turned on "Show others
  which chat I have open" (per workspace, **off by default**), and only for
  chats that have more than one person in them. Off means no `viewing` field,
  not a fake one.
- **Steering** is *not* presence. It is a signed, durable lease (2.3), shown
  from the fold. Presence can't make anyone a steerer.
- Cadence while a shared chat is in front: one beat every 5 s; none while
  the window is in the background or the chat isn't shared. Readers treat
  >12 s as gone.
- Beats stay unsigned (they are cosmetic). The UI never shows presence as
  proof of anything: no "Maya approved" or "Maya is steering" from a beat.

### 2.2 Follow

Local only. "Follow Maya" scrolls your view to the chat and position Maya's
beat names, if she shares viewing. If she doesn't, Follow isn't offered. It
writes nothing.

### 2.3 Steering: a per-chat lease

Steering means: **while a live session is on, only the steerer's messages
start agent turns in this chat; everyone else's messages are comments**.

- New `LeaseScope::Chat` (additive enum variant; `scope_id` = chat id) on the
  existing `CoordinatorLeaseClaimed` / `CoordinatorLeaseReleased` events. No
  new event kind, no renamed id. An older build decodes the event as an
  unknown variant, which folds inert (`Unknown`), so old clients simply don't
  see steering.
- The holder is a **device**, as for every lease; the steerer is the person
  who certified that device (`DeviceCertificate.account_id`). One person can
  steer from one device at a time.
- **Start:** "Start a live session" claims the lease (`reason: Initial`).
  Allowed for the chat's creator and Admin+, and for any Contributor+ in a
  chat with no live session.
- **Hand over:** the steerer picks a person; their client releases with
  `successor` = a device of that person that has a recent beat in this chat,
  and that person's client shows "Maya offers you the steering · Take it /
  Not now". Taking it is the successor's own claim (`reason: Handover`, epoch
  + 1). Until they take it, nobody steers and messages start no turns. If
  they don't take it in 60 s, the offer lapses and the previous steerer may
  reclaim.
- **Take over without an offer:** only the chat's creator or an Admin+, with
  passkey presence (as for other governance actions), and it is shown in the
  transcript: "Ana took over steering from Maya".
- **Lapse:** a steerer whose device stops beating for 60 s loses the lease
  (`lease_health` Lapsed). Then anyone allowed to start may claim.
- **End:** the steerer, the creator or an Admin+ releases with no successor.

**Who enforces it:** the device that runs the turn. In `plan_response` /
`turn_runs_here`, when a chat lease is held at the trigger message's causal
position, a message from anyone but the steerer is not a trigger. The turn
gate (`author_gate.rs`) still applies to the steerer's messages, so steering
never grants more than the steerer's role allows.

### 2.4 Comments on a running turn

A comment is an ordinary `MessageAppended` with an optional, additive
`annotates: Option<Uuid>` (the running assistant message id). It never
starts a turn, is shown next to the running message, and the agent reads it
with the rest of the transcript at its **next** turn. Nothing is injected
into a running turn.

During a live session, non-steerers' messages are comments by default; the
composer says "Comment (Maya is steering)".

### 2.5 Stop

New event `TurnStopRequested { message_id }` (new kind, additive; old
runners ignore it, so the UI says "Asked ‹device› to stop; it may finish if it
runs an older Ligus"). Allowed for the steerer, the person whose message
started the turn, and Admin+. The runner honours it if the turn is still
running when it folds. A stop never deletes the partial message.

### 2.6 Relay load

Budget for one room with **N** people in a shared chat, one streaming turn:

| Source | Requests/s |
|---|---|
| Beats (5 s, foreground only) | N × 0.2 |
| Presence polls (3 s) | N × 0.33 |
| Streaming appends | ~1.3 |
| Pulls after nudges | N × 1.3 |

At N = 10 that's about 19 req/s, under the 50 req/s room limit. At N = 25 it
is about 46, which is too close. So:

- Presence polling drops to 6 s when nobody in the chat is typing, and stops
  while the window is in the background.
- Above 12 people in one chat, viewing beats go to every 15 s and Follow is
  disabled (the UI says why).
- Relay change (follow-up in the relay repository): fire the SSE nudge,
  coalesced to once per second per room, when a presence slot is written, so
  clients can stop polling. Until then, the budget above holds.

### 2.7 Privacy

What a live session adds, beyond today:

| Who | Learns | Mitigation |
|---|---|---|
| Members | which chat you have open, while you share viewing | off by default; per workspace; only shared chats |
| Members | that you steer, hand over, stop | durable by design (it's governance) |
| Relay operator | your device is active (a beat every 5 s, foreground only) | none beyond cadence; slot name already reveals the device id today |
| Relay operator | which chat | nothing: the chat id is inside the sealed beat |

No new plaintext fields reach the relay.

## 3. Tests (executable)

| # | Test | Kind |
|---|---|---|
| T1 | With viewing off, no beat contains `viewing`; with it on, only for multi-person chats. | unit |
| T2 | A forged beat claiming "steering" or someone else's `actorId` never makes the UI show a steerer (steering comes only from the fold). | unit |
| T3 | Property: for any order of {claim A, release→B, claim B, claim C, lapse}, every device agrees on the steerer at every causal position, and only B can take a reserved hand-over. | proptest over orderings (reuse `lease.rs` harness) |
| T4 | A message from a non-steerer during a live session never starts a turn on a 0.3 runner; the steerer's does; the turn gate still refuses a Contributor steerer's write turn. | integration (two devices, test relay) |
| T5 | An old runner (0.2 fixture) ignores the chat lease and starts a turn; the UI on 0.3 shows "steering is advisory here". | compat |
| T6 | `TurnStopRequested` by a non-steerer Contributor is denied at fold; by the steerer, the runner stops and keeps the partial message. | unit + integration |
| T7 | Take-over without an offer needs creator or Admin+ and presence; a Contributor's is denied. | unit |
| T8 | Load test: 25 simulated clients in one room against the relay with the public profile; no 429s with the adaptive cadence. | load (relay repo) |
| T9 | A removed member's device claiming the chat lease after its removal watermark is denied. | unit |

## 4. Adversarial assessment

Stance: the adversarial architecture reviewer, security lens. The first pass
looked at a draft that carried steering in presence beats ("steering: true")
and handed over by a beat from the current steerer.

### First pass: Reject

1. **Steering in unsigned presence** (Critical, high). Beats are unsigned
   and any member can write any slot content they can seal; a forged beat
   would make anyone the steerer. *Revision: steering is a signed, durable
   lease (2.3), test T2.*
2. **Hand-over by assertion** (High, high). "Maya says Ana steers" with no
   acceptance lets the steerer push responsibility onto someone absent.
   *Revision: offer + the successor's own claim, test T3.*
3. **Enforcement where?** (High, high). The draft enforced steering in the
   composer, i.e. on the sender's device. A modified client just sends.
   *Revision: the runner decides (2.3), test T4.*
4. **Stop only on the runner** (Medium, high). A steerer on another device
   can't stop the agent they steer. *Revision: `TurnStopRequested` (2.5), test
   T6.*
5. **Viewing on by default** (Medium, medium). Telling everyone what you
   have open is a real disclosure inside a team. *Revision: off by default
   (2.1), test T1.*
6. **Relay load unbounded** (Medium, medium). Presence counts against the
   per-room limit, and polling scales with N. *Revision: budget and
   adaptive cadence (2.6), test T8.*

### Second pass

#### Decision

**Approve with risks.**

#### Claimed invariants

- S1. Only a signed lease makes someone the steerer; every device agrees who
  it is at each causal position.
- S2. A hand-over completes only when the successor claims it.
- S3. Steering never raises what a person's role allows a turn to do.
- S4. Presence reveals viewing only with consent and never reaches the relay
  in plaintext.
- S5. A live session stays under the relay's per-room limit up to 12 people
  without relay changes.

#### Hidden assumptions

- The device that runs the turn has folded the lease events before the
  trigger message. If not, the lease is "at the trigger's causal position"
  only if the lease events are in its frontier; a lagging runner may start a
  turn for a non-steerer. Same class as today's dispatch races.
- All members' runners are 0.3+; otherwise steering is advisory (S1 holds
  for the display, not for enforcement).

#### Counterexamples considered after revision

| Claim | Sequence | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| S1 | Runner offline, comes back with a backlog of messages from both Maya (steerer) and Ana | only Maya's start turns | correct if lease events are before the messages in canonical order; a message concurrent with the claim is ordered by `(lamport, event_id)` and may count either way | Low | Medium |
| S2 | Successor's claim and the old steerer's reclaim after 60 s race | one steerer | epoch fencing picks one; the other's claim is denied | Low | High |
| S4 | Relay operator times beats | learns activity | learns device activity, not chat | Low | High |
| S3 | Steerer is a Contributor | read-only turns | turn gate still applies | — | High |

#### Required tests

T1-T9. T3, T4 and T5 decide whether steering can be enforced at all.

#### Simpler alternatives

Ship presence (typing, opt-in viewing) and comments on a running turn first,
with no steering at all: anyone allowed to trigger still can. That gives most
of the "together in one chat" value with no new governance. Steering can
follow once the runner-side rule has been proven by T4/T5.

#### Residual risks

- Mixed-version workspaces: steering is advisory until every runner is 0.3+.
- The relay learns when devices are active.
- The per-room limit caps very large live sessions until presence nudges
  ship in the relay.
</content>
</invoke>
