# Workspace guards

Ligus keeps a workspace inside safe bounds with a few guards that work the
same whatever runtime your agents use. You find them in **Settings → Guards**.
They are named after the honeybee colony Ligus takes its name from
(*Apis mellifera ligustica*): colonies post more guards and narrow their
entrance in a dearth, when robbers are about.

## Dearth mode

Dearth mode is a workspace-wide **tight entrance** for hostile periods: an
invite link that leaked, an untrusted repository being processed, a new worker
host you're not sure of yet. An Admin or Owner switches it on with a short
reason; every member then sees a banner at the top of the window until it is
switched off.

While it is on:

- **New members need two Admins.** Joining with an invite on your own is
  suspended. An Admin adds the person, and a second, different Admin approves
  them under *Waiting for a second approval*. Until then they are not a
  member on any device.
- **New devices need a second approval.** A device a member certifies for
  themselves (say a new `ligus serve` box) only counts once an Admin of
  another account approves it. Devices already trusted when dearth mode began
  are unaffected.
- **Agents reach only allow-listed MCP servers.** Everything else is closed:
  no web fetch or search, no shell, no MCP server outside the list. Runtimes
  Ligus can't confine from outside (aider, pi, Hermes, Antigravity, Copilot)
  are refused with a note in the chat. See
  [the sandbox envelope](../concepts/security.md#the-sandbox-envelope-around-every-agent-turn).
- **Protected (brood) paths need a human.** A proposal that changes a
  protected path applies only after a human member approves it themself, even
  if its quorum is met. The default list covers CI and deployment config
  (`.github/**`, `.gitlab-ci.yml`, `.circleci/**`, `Jenkinsfile`, `deploy/**`,
  `infra/**`, ...); edit it as globs, one per line.

The **Entrances** list shows every way outside content or traffic reaches an
agent in this workspace and what dearth mode does to each.

Every one of these rules is checked again on every device when the change
syncs in, so a member who edits their own client can't switch dearth mode
off, approve their own join, or vote as someone else: those events are
dropped everywhere.

Switching dearth mode off returns to normal rules, and anyone still waiting
for a second approval is admitted.
