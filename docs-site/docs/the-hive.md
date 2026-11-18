# The hive behind the names

Ligus and Langstroth are both named after beekeeping, and several Ligus
features borrow from what is known about how honeybee colonies work. This
page explains the names, which piece of the software each part of a hive
stands for, and which colony behaviours inspired which features. The person
who maintains Ligus keeps Italian honeybees, and the aim here is to describe
the bees accurately and the software honestly.

The bees are an inspiration, not a specification. Every feature below is
ordinary engineering that you can read, switch off or watch. Where the
software and the bees part ways, this page says so.

## The names

**Ligus** is short for *ligustica*: *Apis mellifera ligustica*, the Italian
honeybee. The subspecies comes from the Italian peninsula and has long been
popular with beekeepers for its calm temperament, strong brood rearing and
good honey crops. Italians are also known for robbing weaker colonies when
nectar is scarce, which is one reason Ligus has a [dearth mode](#guard-bees-and-a-dearth).
Ligus is the app: the desktop app, the phone app and the `ligus` CLI.

**Langstroth** is named after the Reverend Lorenzo Lorraine Langstroth and
the hive he patented in 1852. Langstroth noticed that bees leave a narrow
gap, roughly a quarter to three-eighths of an inch (about 6 to 9 mm), open
as a walkway. A smaller gap they seal with propolis (plant resin); a larger
one they fill with comb. He called it **bee space**, and built a hive that
kept exactly that gap around each wooden frame. Because the bees didn't glue
the frames in place, a beekeeper could lift any frame out, look at it and put
it back without destroying the comb. Most hives in use today still follow his
design. Langstroth is the platform the app runs on: the relay,
langstroth-auth, the Helm charts and the always-on Ligus hosts.

## The hive: Langstroth, the platform

A Langstroth hive is a stack of simple parts: a bottom board with an
entrance, one or more boxes, frames hanging inside the boxes, and a cover on
top. The platform has a similar shape.

| Hive part | What it does in a hive | In Langstroth |
|---|---|---|
| **Frames and bee space** | Each frame hangs separately with bee space around it, so it can be lifted out and inspected. | Each platform piece is a separate program you can run, read and replace on its own: the relay, langstroth-auth, the `ligus` hosts, the charts. The layers keep a clear gap between them: langstroth-auth decides who can reach the relay, the workspace's signed roster decides who is a member, and the workspace key decides who can read anything. One never stands in for another. See [langstroth-auth](operate/deploy/ligus-auth.md#what-it-does-and-what-it-never-does). |
| **Boxes** | The colony lives in a stack of boxes; the beekeeper adds a box when the bees need room. | Hosts. A `ligus serve` box is the always-on home of a workspace, a member of it in its own right, and keeps agents answering while laptops sleep. `ligus worker` boxes add capacity for agents. See [Running ligus serve](operate/ops/serve-box.md) and [Headless agents](operate/concepts/headless-agents.md). |
| **The apiary** | A yard of hives, set out and tended together. | Kubernetes (preview). The Helm charts run the relay (and optionally langstroth-auth) in one namespace and each workspace's `ligus serve`, workers and MCP servers in a namespace of their own. See [Kubernetes](operate/deploy/kubernetes.md). |
| **The entrance** | The one way in and out, where traffic is easy to watch. | The relay. Devices sync through it, and it can tunnel the phone to a `ligus serve` box. See [Self-hosting a relay](operate/networking/self-host.md). |
| **Guard bees** | Guards stand at the entrance and check arriving bees, mostly by odour. | Checks at two places. At the relay, tokens (and langstroth-auth sign-in, if you run it) decide who may reach which workspace. Inside, every device re-checks each synced event against the signed member list and drops what doesn't belong, so the relay alone can't let anyone in. See [Authorization on ingest](ligus/concepts/security.md#authorization-is-enforced-on-ingest-not-just-at-authoring). |
| **The outer cover** | Anyone walking through the apiary sees the outside of the boxes, never the comb. | End-to-end encryption. The relay is content-blind: it stores and forwards ciphertext and sees metadata (which workspaces exist, sizes, timing), never a workspace key. See [The relay is content-blind](ligus/concepts/security.md#the-relay-is-content-blind). |

Some platform pieces have no honest hive counterpart, so they keep their
plain names:

- **LLM gateways.** A host can send its agents' model traffic through a
  gateway you already run (LiteLLM, kgateway, Envoy AI Gateway and others),
  with credentials the agents never see. See
  [Deploying with an LLM gateway](operate/deploy/model-gateways.md).
- **Deploying with an agent.** The repository carries instructions, skills
  and non-interactive tools so a coding agent can do a deployment, stopping
  for you wherever a person must decide. See
  [Deploying with an agent](operate/deploy/with-an-agent.md).

## The colony: Ligus, the app

A honeybee colony has no manager. No bee, not even the queen, directs the
work. Tens of thousands of workers each respond to local cues, and the
colony still finds food, picks homes, defends itself and keeps the brood
warm. Ligus borrows a few of those mechanisms for a workspace where people
and agents share work.

### Scouts and quorum: choosing between options

**The bees.** When a swarm leaves its old nest, it hangs in a cluster while
a few hundred scouts search for a new home. Thomas Seeley's studies of
nest-site selection (described in his book *Honeybee Democracy*) show how
they decide: a scout that finds a good site dances for it, more
enthusiastically for a better site, and her dancing for it fades with each
return trip unless the site keeps impressing new scouts. The swarm commits
when enough scouts are at one site at the same time, a quorum, not when
every scout agrees. Scouts at the chosen site then make a piping sound that
tells the swarm to warm up its flight muscles before take-off.

**In Ligus.** A [proposal set](ligus/features/proposal-sets.md) puts several
alternative answers to one question side by side. Agents act as scouts: each
assesses one alternative blind and scores it. Support
[fades every round](ligus/features/proposal-sets.md#support-fades-unless-its-renewed)
unless a scout renews it. When enough different scouts back one alternative
above the quality bar, the set moves to *piping*: that alternative is the
front-runner and the decision goes to a person. The move happens in two
steps, like the warm-up before a swarm flies: a
[pre-flight](ligus/features/proposal-sets.md#deciding-pre-flight-then-apply)
checks tests, the base branch and the hosts, and only then can the change be
applied.

The analogy stops at the decision. Swarm scouts are sisters with every
reason to report honestly; agents can be wrong or manipulated. So in Ligus a
quorum of agents only nominates, and a person decides.

### Stop signals

**The bees.** A bee can deliver a stop signal: a short vibrating pulse,
often given while butting her head against a dancer, which makes the dancer
more likely to stop. It was first studied in foraging bees; Seeley and
colleagues later showed that during nest-site selection, scouts for one site
deliver stop signals to scouts dancing for rival sites. This cross-inhibition
helps a swarm reach a decision instead of splitting between two good sites.

**In Ligus.** When a proposal set reaches quorum, each rival alternative is
frozen and its scouts get a
[stop signal](ligus/features/proposal-sets.md#quorum-the-front-runner-and-stop-signals)
in the chat asking them to stand down. If nothing reaches quorum by the
scouting deadline, the best-supported alternative wins and its rivals are
stopped the same way, so two equal options don't stall the decision.

### Genetic diversity: different minds on one question

**The bees.** A queen mates with many drones, so a colony is a mix of
half-sister families with different tendencies. Heather Mattila and Seeley
found that genetically diverse colonies did better than uniform ones at
building comb and foraging.

**In Ligus.** Scouts on a model or runtime that hasn't assessed an
alternative yet [go first](ligus/features/proposal-sets.md#scouting), and the
set view warns when all of an alternative's scouts ran on one runtime. The
[Roster](ligus/features/flow.md#careers) flags a monoculture, a roster whose
agents all run on one runtime and so tend to fail, and agree, together.

### The waggle dance: telling others where to go

**The bees.** Karl von Frisch showed that a forager returning from a good
food source performs the waggle dance on the comb. The angle of the waggle
run relative to vertical gives the direction of the food relative to the sun,
and the length of the waggle run gives its distance. Bees dance longer for
richer sources, so more recruits end up at better ones, and the colony's
foragers spread across patches roughly in proportion to how good each is.
Not every forager follows dances; some search on their own, which is how new
patches get found.

**In Ligus.** Two features carry pieces of this.

- A [hand-off card](ligus/features/flow.md#hand-off-cards-and-context-parcels)
  tells the agent receiving work where it is (the branch and files) and how
  good it was (measured test results and proposal state). An agent can attach
  a **context parcel**, a short summary of key files, decisions and dead
  ends, so the receiver starts primed instead of rereading the chat.
- [Routing](ligus/features/flow.md#routing-un-addressed-work) of
  un-addressed messages keeps a score per agent and task class, built only
  from outcomes Ligus measured itself (the code calls these dance scores),
  and picks an agent with probability in proportion to its score rather than
  always the top one. A share of picks
  [explores](ligus/features/flow.md#exploration) other agents on purpose.
  Routing ships in shadow mode: it records what it would have picked until
  you switch it to live.

The dance's actual code, an angle and a duration, has no counterpart in
Ligus; only the idea of advertising where good work is, weighted by how good
it turned out, carries over.

### The tremble dance: when unloading backs up

**The bees.** A returning nectar forager hands her load to a younger
receiver bee inside the nest. Seeley found that when foragers have to wait a
long time to find a receiver, they perform a tremble dance instead of a
waggle dance. Trembling recruits more bees to receiving nectar, and the
stop signals that often go with it damp recruitment of yet more foragers.
The colony balances collecting against processing without anyone counting.

**In Ligus.** The review gate is where agents' work waits for people.
[Backpressure](ligus/features/flow.md#backpressure-the-review-queue)
measures how long proposals and hand-offs wait. When the review queue is over
its limit, agent turns are told to finish open proposals before opening new
ones (fewer foragers), and with *Ask more reviewers* on, members who could
review are notified (more receivers). The code module is named after the
tremble dance.

### Response thresholds: who picks up unassigned work

**The bees.** Workers differ in how strong a cue must be before they take up
a task. In the response-threshold model studied by Gene Robinson, Robert
Page and others, a bee with a low threshold for a task starts it first; if
the need keeps growing, bees with higher thresholds join. Because thresholds
vary, the colony responds in proportion to the need instead of all at once.

**In Ligus.** Work nobody was asked to do (a failing test run, an unreviewed
proposal, a failed workflow stage) lands on the
[task board](ligus/features/flow.md#the-task-board). Each item's stimulus
grows with its age and severity, each agent has its own threshold per kind of
work with a small per-agent offset, and the free agent whose threshold is
crossed by the most is suggested.

### Age and careers

**The bees.** A worker usually starts with tasks inside the nest, such as
cleaning cells and feeding brood, and moves on to guarding and foraging as
she ages. The schedule bends to the colony's needs: when foragers are lost,
younger bees can start foraging early, and foragers can go back to nursing.

**In Ligus.** A new agent starts on
[probation](ligus/features/flow.md#careers) with low-risk work such as
reviews, docs and tests, and graduates once enough of its proposals have been
approved. When the review queue is long, the strongest proposer is moved back
to reviewer duty until it drains. Careers are recomputed from the log, so
they reverse on their own.

### Scent marks on the flowers

**The bees.** Foraging bees leave faint scent marks on the flowers they have
just visited, and other bees tend to pass over a recently visited flower,
which saves them a trip to an empty one.

**In Ligus.** Agents and people leave
[workspace marks](ligus/features/flow.md#workspace-marks) on files: *claim*
("I'm working on this"), *under construction* (a lock on a file or lines) and
*needs review*. Claims and locks expire on their own, the way a scent fades.
Agents see active marks in their context and can work around them without
anyone having to mention them.

### Guard bees and a dearth

**The bees.** Guards at the entrance check arriving bees and challenge ones
that don't smell like nestmates. In a dearth, when little nectar is coming
in, bees from other colonies try to rob stored honey, and colonies guard
their entrances more closely. Beekeepers help by narrowing the entrance so
there is less to defend.

**In Ligus.** [Dearth mode](ligus/features/guards.md#dearth-mode) is a
workspace-wide narrow entrance for a risky period, such as a leaked invite
link or an untrusted repository. While it is on, a new member needs two
Admins, a new device needs a second approval, agents reach only allow-listed
MCP servers, and changes to protected paths (CI and deployment config, which
Ligus calls brood paths) need a human's own approval. Every device re-checks
these rules when events sync in.

### Resin and lockdown

**The bees.** Colonies coat the inside of the nest with propolis, resin
collected from plants, which has antimicrobial properties. Marla Spivak's
group found that colonies challenged with a fungal brood disease collected
more resin, a form of what researchers call social immunity.

**In Ligus.** A chat [locks itself down](ligus/features/guards.md#chat-lockdown)
when an agent turn shows a sign of infection: a prompt-injection marker in
something the agent read, a credential in its changes, or a CI change nobody
asked for. Agents in a locked chat lose network and MCP access, and every
proposal needs a human's approval, until a member clears it.

### Hygienic behaviour

**The bees.** Some colonies detect diseased or dead brood under the cappings,
uncap the cells and remove it before it can spread. Walter Rothenbuhler's
work on American foulbrood showed this hygienic behaviour is inherited, and
breeders now select for it.

**In Ligus.** The [hygiene sweep](ligus/features/guards.md#hygiene) runs
shortly after launch and every six hours. It removes worktrees left by
interrupted turns (saving unfinished edits as a patch first), cleans up after
deleted chats, and marks proposals whose diff no longer applies as stale.

### Brood temperature and plan windows

**The bees.** Workers keep the brood nest within a narrow temperature band,
warming it by shivering their flight muscles and cooling it by fanning.
Julia Jones, Ben Oldroyd and colleagues found that genetically diverse
colonies hold temperature steadier, because bees with different thresholds
start fanning at slightly different temperatures instead of all at once.

**In Ligus.** Subscription runtimes have usage windows.
[Plan windows](ligus/features/guards.md#plan-windows-staggered-throttles)
back off in steps as a window fills: scheduled runs pause first, then
workflow stages and agent-to-agent hand-offs, and each agent's threshold is
offset a little so agents pause one at a time. Your own requests are the
brood and always run.

### Leaving a patch

**The bees.** Foraging theory predicts that a forager should leave a patch
of flowers once what she gains there drops below what she could expect
elsewhere, and foraging bees broadly behave this way.

**In Ligus.** When an agent works through Ligus's own tools, Ligus measures
progress (a write that changes a file, more tests passing) instead of taking
the agent's word for it. After a run of tool calls with no measurable
progress, the [patch-leaving rule](ligus/features/guards.md#long-agent-loops-the-patch-leaving-rule)
asks the agent to stop, explain what is blocking it and ask you how to
continue.

### Starting in a nuc

**The beekeeping.** Beekeepers often start a colony in a small nucleus box,
a nuc, and give it more room only once the bees have drawn out most of the
frames they have. Empty comb that the bees can't patrol invites pests.

**In Ligus.** A workspace starts with one agent on your device, and
[growth suggestions](ligus/features/guards.md#growing-the-workspace-when-its-ready)
appear only when the workspace's own use calls for them: an always-on host
when requests keep waiting for an offline machine, a worker when agents keep
pausing near a plan window's limit, another agent when one agent does nearly
all the work.

### Not built

Some colony behaviours have no Ligus feature in 0.1.0, and this page doesn't
claim them: there is no smoker (a calming signal for agents), no
trophallaxis-style sharing of state between agents beyond hand-off cards and
context parcels, no queen and no pheromone-based coordination. If one of
these appears in Ligus later, it will be documented with the feature.

## Glossary

| Bee or hive term | Ligus or Langstroth term |
|---|---|
| Bee space, removable frames | Separate, replaceable platform pieces with clear boundaries between reach, membership and content |
| Hive box | A host: `ligus serve` (the home box) or `ligus worker` |
| Apiary | A Kubernetes cluster running the Helm charts |
| Entrance | The relay |
| Guard bees | Relay tokens and langstroth-auth; on-ingest checks on every device |
| Outer cover | End-to-end encryption; the content-blind relay |
| Nest-site scouts | Scouts in a [proposal set](ligus/features/proposal-sets.md) |
| Quorum | Agent quorum in a proposal set (nominates; a person decides) |
| Piping | A proposal set's piping phase, before pre-flight |
| Stop signal | Stop signal to a rival alternative's scouts |
| Waggle dance | [Hand-off card](ligus/features/flow.md#hand-off-cards-and-context-parcels) and context parcel; outcome scores for [routing](ligus/features/flow.md#routing-un-addressed-work) |
| Tremble dance | [Review-queue backpressure](ligus/features/flow.md#backpressure-the-review-queue) |
| Response threshold | The [task board](ligus/features/flow.md#the-task-board) |
| Age-based tasks | Agent [careers](ligus/features/flow.md#careers): probation, graduated, reviewer duty |
| Scent mark | [Workspace mark](ligus/features/flow.md#workspace-marks): claim, lock, needs review |
| Dearth, narrowed entrance | [Dearth mode](ligus/features/guards.md#dearth-mode) |
| Brood | Protected (brood) paths; your own requests under plan windows |
| Propolis, social immunity | [Chat lockdown](ligus/features/guards.md#chat-lockdown) |
| Hygienic behaviour | The [hygiene sweep](ligus/features/guards.md#hygiene) |
| Brood-nest thermoregulation | [Plan windows](ligus/features/guards.md#plan-windows-staggered-throttles) |
| Leaving a flower patch | The [patch-leaving rule](ligus/features/guards.md#long-agent-loops-the-patch-leaving-rule) |
| Nuc | A new workspace; [growth suggestions](ligus/features/guards.md#growing-the-workspace-when-its-ready) |
