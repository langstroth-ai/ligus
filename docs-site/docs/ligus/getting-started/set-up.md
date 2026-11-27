# Set up

Seven guided flows set up the parts of Ligus that used to take several
screens or commands. They are the same in the desktop, in the `ligus` CLI
and (next) in `lt`, because one engine runs them.

| Flow | Desktop (⌘K, or `/setup` in the composer) | CLI |
|---|---|---|
| An agent, in one step | **Add an agent** | `ligus setup agent` |
| An MCP server your agents can call | **Connect a tool** | `ligus setup tool` |
| A git checkout bound to a workspace repository | **Add a code folder** | `ligus setup folder` |
| An always-on `ligus serve` box | **Add a box** | `ligus setup box` (on the box) |
| Your phone | **Add a phone** | `ligus setup phone` |
| A teammate | **Invite a person** | `ligus setup invite` |
| The relay the workspace syncs through | **Set up a relay** | `ligus setup relay` |

`ligus setup` on its own lists the flows and what each has already done on
this device. In the desktop, an empty list in the **Tools** pane (runtimes, MCP
servers, hosts) offers its flow as its one action.

## How every flow behaves

- **It checks first.** Each flow starts with checks (the agent sandbox, an
  installed and signed-in CLI, your role in the workspace, the relay) and
  writes nothing until every check has passed.
- **It can be run again.** A step that is already done is skipped and says
  so: running **Add an agent** twice with the same name, source and
  model changes nothing.
- **It never leaves half a setup.** Every input is validated before the
  first write. When a later write fails, the flow undoes the writes it
  made (the runtime it defined for an agent, the token it saved for a
  tool) and says what it undid.
- **A failure says how to fix it.** The line under a failed check is the
  fix, often a command (`codex login`, `ligus doctor --sandbox`,
  `langstroth-relay check --url …`).
- **Some steps are a person's.** Approving a box with a passkey, pairing a
  phone and creating an invite happen in their own screens; a flow stops
  there (the CLI exits 4) and checks again afterwards. No flag or setting
  can do them for you.
- **Secrets stay in the secret store.** A tool's bearer token, a relay
  access token and a box's admission code are typed into a masked field on
  the desktop, or read from stdin (`-`) or an owner-only file (`@FILE`) on
  the CLI, never from the command line. They go to this device's secret
  store and are never shown, logged or returned.

## Who may run what

| Flow | Needs |
|---|---|
| Add an agent | Contributor on your own devices; placing it on a host, or defining a workspace runtime for it, needs an Admin |
| Connect a tool | Contributor (a tool reaches other people's prompts through shared agents) |
| Add a code folder | Contributor to bind; Admin to add a repository to the workspace |
| Add a box | on the desktop: Admin, with a passkey; on the box: nothing (it only asks) |
| Add a phone | your own phone, any member |
| Invite a person | Admin |
| Set up a relay | any member, for this device |

While a workspace is in lockdown, **Connect a tool** accepts only a server
the lockdown allows.

## The flows

### Add an agent

One sheet, the same in the desktop (**Add an agent** in ⌘K, `/setup agent`,
the **Tools** pane and the first run), in `lt` and in `ligus setup agent`:

1. **Name**: one word, not a reserved mention name such as `ligus` or
   `all`, and not another agent's or member's name.
2. **What answers**: one list, each entry with its state:
   - a coding CLI (Claude Code, Codex, GitHub Copilot CLI, Google
     Antigravity CLI, Aider, Pi) on this device or on a host: "Claude Code ·
     signed in · this Mac", "Codex · signed in · build-1". The sign-in state
     on this device is checked without reading any token; a host's comes
     from its own report;
   - a model gateway a host offers: "gw-home (OpenAI API) · offered by
     box-1";
   - a hosted API whose key a host holds: "team-claude (Anthropic API) · key
     by secret reference on box-1". The agent names the provider as the host
     lists it; the host turns that name into the secret its `host.toml`
     declares for this workspace, and into nothing else (see
     [What a host offers](../reference/cli.md#what-a-host-offers));
   - a model server this device reaches (Ollama at
     `http://localhost:11434` by default, or another OpenAI-compatible
     server; plain `http://` only on this machine);
   - in the desktop, an API key kept on this device (Anthropic, or an
     OpenAI-compatible API). The key goes to this device's secret store,
     never to the workspace.

   Hosts are the boxes and workers of the workspace and your own other
   devices; other people's computers aren't listed. The best entry is
   picked for you: a CLI signed in here first.
3. **Model**: that source's own list. A gateway's list is the one its host
   reports (with `auto` when the gateway lists it). A coding CLI can stay
   on its own default; everything else needs a model.
4. **Runs on**: this device for a CLI signed in here, or for a model server
   or a key here; the only host that offers a gateway or a hosted API (you
   pick when several do). One line below says what happens: "Turns run on
   this device while Ligus is open." or "Turns run on box-1. When it's
   offline, @coder waits." A host that doesn't offer the source can't be
   picked.
5. **Run a test turn**: on by default for your first agent in the desktop
   and `lt` (`--test-turn` on the CLI). It runs on this device only; an
   agent on a host is tested by asking it something in a chat.

One button, **Add @name**, writes the agent with where it runs in one
record. Underneath, the agent still names a runtime: the sheet reuses one
that runs the same source and model, or defines one. In a shared workspace
the new agent needs your passkey. An agent on a host needs a workspace
runtime the host can run; when a new one is defined, that is a second
confirmation, and the sheet says **Confirm 1 of 2**, then **2 of 2**. A
reused runtime takes no second one.

Without an agent sandbox on this device a coding CLI can't run here, and
the flow says what is missing; it never offers to turn the sandbox off.
For Antigravity, **May run local servers** is offered (off by default; the
Owner's). See [Configuring a runtime](configuring-a-runtime.md),
[Offline and local models](offline-local-models.md) and
[Model gateways](../../operate/deploy/model-gateways.md#choosing-a-gateway-for-an-agent).

On the CLI:

```sh
ligus setup agent --name builder --source claude-code --test-turn
ligus setup agent --name coder --source gateway:gw-home --model auto --runs-on box-1
ligus setup agent --name local --source local --model llama3.2
```

`--source` is `claude-code`, `codex`, `copilot`, `antigravity`, `aider`,
`pi`, `gateway:NAME`, `provider:NAME` or `local`. `--runs-on` is `here`, or
a host's id or name (`ligus hosts` lists them and what each offers). The
older `--harness H` and `--gateway NAME` still work and mean `--source`.

### Connect a tool

An MCP server is a command on this device or an `https://` URL (plain
`http://` only for a server on this machine). A URL with a user name or a
token in it, or a command argument that looks like a token, is refused:
give a URL server its token in the **Bearer token** field, which saves it
in the secret store bound to that URL's origin. The flow starts the server
once under the agent sandbox and lists its tools before anything is saved,
then saves the server and **which agents may use it** in one step. That
choice is kept on this device, never in a file a commit could change, and
the default agent (`@ligus`) is offered only tools every agent may use. See
[MCP servers](../addons/mcp.md).

### Add a code folder

Pick a git checkout. The flow checks it is a main checkout (not a linked
worktree), that another host on this machine doesn't manage it and that it
isn't bound to another repository, then matches it to the workspace's
repository by its remote, adding the repository (with its default branch:
`origin`'s HEAD, else `main` or `master`) when you are an Admin, and binds
it. See [Repositories](../features/repositories.md).

### Add a box

On the box, with its own data dir (never the desktop app's):

```sh
LIGUS_DATA_DIR=~/.ligus-serve ligus setup box --label home
```

It checks the agent sandbox, prints the box's join request and its
fingerprint, and stops. On the desktop, **Add a box** opens **Admit a
box**: paste the request, check the fingerprint matches, approve with a
passkey, and send the sealed code back. Then, on the box:

```sh
ligus setup box --code - < ./admission
```

It keeps the admission and checks that `ligus serve` answers. See
[Guided setup](../reference/cli.md#setup) and `ligus help box`.

### Add a phone

The desktop checks this device can pair a phone (a shared workspace, a
reachable box) and opens **Add a phone**, which shows the pairing code in
your own window. On a box, `ligus setup phone` checks the box and then asks
you to run `ligus pair` yourself, in your own terminal: it prints a secret
link, so never in a job or a logged session. See
[The phone app](../features/phone-app.md).

### Invite a person

The flow checks you are an Admin and the workspace has a relay, and says
what the person may do (they join as a Contributor; an Admin can change
that in **People**). **Create invite** makes a single-use code shown once
in the wizard to copy; send it only to the person it is for. Run the flow
again to see whether it was accepted. The CLI never creates an invite
(the code carries the workspace key); `ligus setup invite` lists your
invites and which were accepted. See [Invites](../features/invites.md).

### Set up a relay

Choose the community relay (`relay.langstroth.ai`, best-effort) or your
own. The flow checks the relay (reachable, your access token accepted, or
nothing stored for this workspace yet), switches this device to it, and
checks again. Only `https://` relays are accepted, except one on this
machine (`localhost`, `127.0.0.1`, `::1`). Every member's devices must use
the same relay, and new invites carry it. See
[The community relay](../../operate/networking/community-relay.md) and
[Self-hosting a relay](../../operate/networking/self-host.md).

## For scripts and coding agents

Every field of a flow is a flag (`ligus setup <flow> --help` lists them),
nothing prompts, `--dry-run` checks and validates without writing, and
`--json` prints one [`ligus.checks.v1`](../../operate/reference/cli-conventions.md)
report with `flow`, `done`, `dryRun` and `results` added. Exit codes follow
the shared table: 0 done, 1 failed, 2 a missing or invalid flag, 3 a tool,
sign-in or sandbox to fix, 4 a person must act, 6 refused by your role or
the lockdown, 7 the relay or server didn't answer.

In a workspace with stricter device rules, some writes (a new agent, a workspace
runtime for it, a repository) need your passkey. The CLI has none, so it
sends each change to your own desktop or phone and waits (two in a row,
"passkey 1 of 2", when an agent on a host needs a new workspace runtime), printing what it is and a
code: "Confirm on your desktop: Define workspace runtime “builder” (…) (code
K7Q4 M9X2)". Your desktop's Inbox shows the same change and code; confirm it
with your passkey there (Ctrl-C or `--timeout` cancels; `--no-wait` exits 4
and `ligus handoff wait` finishes it, see
[Steps that need your passkey](../reference/cli.md#handoff)). If the relay
has no approval mailbox, the step stops with exit 4 as before: do it in the
desktop app, or run the flow again with `--recovery-words-file PATH` (your
recovery key's 24 words, in a file only you can read).

```sh
ligus setup agent --source claude-code --name builder --json | jq '.done'
ligus setup tool --id notes --command "npx -y @modelcontextprotocol/server-filesystem ~/notes" --reach builder
```

A coding agent on your machine can run `ligus setup` like any other
command, and stops at exit 4 like you would. Ligus's own agents (`@ligus`
and the agents in a chat) can't start a flow.
