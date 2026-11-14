# Exploring options: proposal sets

A normal [proposal](multi-agent.md#agent-authored-proposals) is judged on
its own. When there is more than one reasonable answer (which cache, which
API shape, which of two fixes), Ligus can put the answers side by side as a
**proposal set**: one question, several **alternatives**, each assessed
independently by agents acting as **scouts**, with a person making the final
call.

The design follows how a honeybee swarm chooses a new nest. Scouts each
inspect one site and report on it; their enthusiasm fades unless they go
back and confirm it; the swarm commits once enough scouts are at one site at
the same time (a *quorum*); scouts at the winning site signal the others to
stop; and the move happens in two steps, warming up first and only then
leaving.

## Opening a set

- **Ask for options.** Write something like *"Explore options for caching
  the search results"*, or type `/explore` in the composer. The agent answers
  with 2 to 6 alternatives instead of picking one.
- **Competing proposals.** When your message asks for options and two
  different agents each file a proposal in answer, Ligus groups them into a
  set. Ordinary requests are never grouped: two agents editing for one
  request are usually doing complementary work.

Agents open a set by ending a reply with an `alternatives` directive:

```text
[[alternatives: {"question": "Which cache?", "taskClass": "design",
  "options": [{"title": "Redis", "body": "…"},
              {"title": "In-process LRU", "body": "…"}]}]]
```

Each option becomes an ordinary proposal (it keeps its votes, notes and
revisions) that belongs to the set. Sets appear at the top of the **Review**
pane, and their alternatives are shown inside the set rather than as separate
cards.

## Scouting

Click **Send scouts**. Ligus picks scouts from the chat's agents (and the
primary runtime) and sends each one **a single alternative** to assess:

- **Blind.** A scout sees only the alternative it was given, never another
  scout's score. Its reply in the chat is replaced by a short note ("posted a
  scout assessment of B") and the score and rationale live in the set, so the
  next scout can't read them in the transcript.
- **Not its own work.** A scout never assesses an alternative it produced,
  and if one does, the assessment is shown but not counted.
- **Diverse.** For each alternative, scouts on a model or runtime not yet used
  there go first. Agents on the same model share blind spots, so a local
  model, Claude Code and Copilot assessing the same option are worth more than
  three copies of one.

A scout ends its reply with a score from 0 to 100:

```text
[[assess: {"alternative": "<alternative id>", "score": 82, "rationale": "…"}]]
```

The set view shows a score only once a scout has posted it; a scout that is
still working shows as *assessing…*.

## Support fades unless it's renewed

Each assessment loses support every **evaluation round** (25 points per round
by default), so an early opinion can't lock the set. A round starts when
there is new evidence:

- a person comments on, or asks for changes to, an alternative;
- an alternative is revised (the new version takes the old one's place);
- a pre-flight fails (see below);
- you click **New round**.

A scout renews its support by assessing again. **People's votes never fade.**
Votes are the human gate, not scouting.

## Quorum, the front-runner and stop signals

An alternative reaches **quorum** when enough different scouts (*k*) each
still support it at or above the **quality bar**. The set then moves to
**piping**:

- that alternative is marked the **front-runner**;
- the other alternatives are **frozen**: no new scout work, and they can't be
  sent back to an agent for revision;
- each rival's scouts get a **stop signal** in the chat, asking them to stand
  down (or re-evaluate if the set returns to scouting);
- the decision is handed to you.

If nothing reaches quorum by the **scouting deadline** (30 minutes after the
set opens), the alternative with the most support becomes the front-runner and
its rivals are stopped the same way. Two equally good options shouldn't stall
the decision. An exact tie goes to the alternative proposed first.

Quorum among agents only nominates. It never applies anything.

## Fast or careful

The **Quorum** dial on each set sets how much agreement is needed:

| Position | Scouts (*k*) | Quality bar | Human approvals |
|---|---|---|---|
| Fast | 1 | 60 | 1 |
| Balanced (default) | 2 | 70 | 1 |
| Careful | 3 | 80 | 2 |

Approvals count only from members at or above the set's role floor
(Contributor by default), and nobody's approval counts on an alternative they
authored. One qualifying rejection still rejects an alternative outright.
Changing the dial needs the same role as deciding.

## Deciding: pre-flight, then apply

Choosing happens in two steps, so a stale or broken change can't land:

1. **Approve** the alternative you want (any active one, not only the
   front-runner).
2. **Run pre-flight.** Ligus checks, and records in the set:
    - **Tests**: your pre-flight test command runs in a scratch worktree with
      the change applied. Set the command with `LIGUS_PREFLIGHT_TEST`. It is
      read from your machine only, never synced and never taken from the
      repository (not even its git config), so neither a teammate nor a repo
      can make your machine run a command. It runs in the agent sandbox:
      writes only in the scratch worktree, no network unless
      `LIGUS_SANDBOX_EGRESS_ALLOW` names hosts. With no command set, the
      check is skipped.
    - **Base unchanged**: the chat's branch hasn't moved since the set
      opened, and the change still applies cleanly.
    - **Hosts online**: the hosts needed to carry it out are online.
3. **Apply.** Only after a passing pre-flight. The change is applied exactly
   as Implement applies a proposal, and the set is marked decided.

The dial sets the approvals every alternative needs, but it only ever raises
an alternative's own bar (a [core path zone](flow.md#path-zones) keeps its
stricter one). In a locked-down chat, or in dearth mode for a change to a
protected path, the pre-flight and the apply also wait for a human member's
own approval ([workspace guards](guards.md)); that wait never counts as a
failed pre-flight.

**A failed pre-flight sends the set back to scouting.** Rivals thaw, the
round advances (so older support fades), and scouts can assess again. A failed
apply counts as a failed pre-flight.

You can also **Close without choosing**, **Withdraw** an alternative for good,
or **Revert** an applied file change from the decided set.

## What agents can't do

Agents can open a set and score alternatives. They can't vote, move the dial,
run a pre-flight, apply, or decide: those are person-only actions in the set
view. The decision only takes effect when the chosen alternative has the
required human approvals and a passing pre-flight, and every device checks
this when it replays the set's history. A set alternative can't be applied
with a plain **Implement** either.

Every step of a set (opening it, assessments, quorum and stop signals, the
dial, pre-flight results, the decision and a revert) is also re-checked
against the workspace's member list when a device replays the chat. A step
signed by someone below Contributor, below the set's decision role, or by a
device whose owner isn't a member is ignored on every device.

## Older versions and the phone

Proposal sets are new in 2.0. Devices on an older version ignore the set
and see the alternatives as ordinary proposals. The phone shows which set a proposal
belongs to, and `ligus serve` never applies a set alternative on approval
(even with `LIGUS_SERVE_APPLY_ON_APPROVE`): apply it from the desktop's set
view.

## Where the bee analogy stops

- **Scores are agent claims.** Bees are honest because they are related;
  agents can be wrong or manipulated. That is why agent quorum only nominates,
  and the measured checks (tests, base, hosts) and a person's approval decide.
- **A few scouts, not hundreds.** With two or three agents, a quorum is noisy,
  and agents on the same model agree for the same wrong reasons. Use diverse
  scouts, and treat a single-model quorum with care (the set view says when
  all of an alternative's scouts ran on one runtime).
- **Minutes, not days.** The decay rate and deadline are tuned for people
  waiting on an answer.
