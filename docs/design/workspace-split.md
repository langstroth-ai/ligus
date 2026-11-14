# Guided workspace split (design note)

Status: design only, not implemented (Ligus 2.0). Companion to the
coordinator leases, dearth mode and lockdown work in 2.0.

## Why

A honeybee colony that outgrows its cavity doesn't keep growing: it swarms.
The old queen leaves with a large share of the workers and a daughter queen
inherits the nest. Supersedure (replacing a failing coordinator in place) and
swarming (splitting the group) are different events with different
inheritance rules. Ligus 2.0 implements the first (planned lease handover).
This note covers the second: a sub-team takes a subset of chats, agents and
repositories into a **new workspace with fresh keys**, the parent keeps its
identity, and history before the split stays readable to both.

It didn't fit in 2.0 because it touches the key schedule, the relay room
model and membership at once, and each needs its own review.

## What a split must guarantee

1. **Fresh keys.** The child workspace gets a new workspace key and a new
   relay room. Nothing in the parent's room can read the child's events after
   the split, and vice versa.
2. **Parent identity unchanged.** The parent keeps its room, key, founder pin
   and config log. Members who leave are removed from the parent through the
   ordinary `MemberRemoved` path, followed by the usual key rotation that
   excludes their devices.
3. **Pre-split history readable to both.** Chats that move are *copied* into
   the child as a compaction-style seed snapshot (the founder of the child
   signs it, satisfying the self-consistent-seed rule), carrying the
   transcript and proposals up to the split point. The parent keeps the
   originals, read-only for the moved chats (archived with a pointer to the
   child).
4. **No silent re-founding.** The child is a new workspace with its own
   founder (the Admin who runs the split), pinned in its invite like any
   other. A forged "split" event can't move anything: the split is a local
   action that creates new logs, never an event that rewrites the parent.

## Shape

- A `WorkspaceSplitPlanned { plan_id, chats, agents, members, child_name }`
  event on the parent's config log (Admin+, re-checked on ingest) records the
  intent and lets every member see it. It changes no state by itself.
- The Admin running the split creates the child workspace locally (new room,
  new key, new config seed with the leaving members, agents and runtimes),
  copies each moved chat as a seed snapshot into the child, and posts invites
  sealed to the leaving members' devices (the existing invite-by-handle path).
- A `WorkspaceSplitCompleted { plan_id, child_room_hint }` on the parent marks
  the moved chats archived with a pointer, removes the leaving members, and is
  followed by a parent key rotation that excludes their devices.
- Coordinator leases for workflow runs in moved chats are released in the
  parent before the copy; runs restart in the child under fresh leases.

## Open questions

- Repository bindings are device-local; the split can only suggest which
  checkout each child device should bind.
- Whether pre-split history in the child should stay verifiable against the
  parent's signatures (keep original envelopes) or only as a signed snapshot
  (simpler, loses per-event provenance).
- Signals that suggest a split (two clusters of members and agents with few
  hand-offs between them, coordinator dilution as the workspace grows) are an
  observability question and out of scope here.
