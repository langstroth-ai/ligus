# CLI conventions

Every Ligus command-line tool reports checks the same way: `ligus doctor`,
`langstroth-relay check`, and the deploy scripts `deploy/local/ligus-local`
and `deploy/kubernetes/ligus-k8s`. A person reads the text form; a script or a
coding agent reads one JSON document with `--json`. `ligus help checks` prints
this page in short.

Data on stdout, everything else on stderr: when a command's text output is a
code meant for pasting elsewhere (`ligus devices request`), stdout holds only
that code, and the fingerprint, instructions and progress go to stderr, so
`ligus devices request | pbcopy` copies exactly the code. A secret is never
printed or asked for on the command line: it is read from a file, from stdin
(`-`), or at a terminal from a hidden paste or the clipboard.

## The `ligus.checks.v1` report {#checks-v1}

With `--json`, a check command prints exactly one JSON document on stdout
(progress and errors go to stderr):

```json
{
  "schema": "ligus.checks.v1",
  "tool": "ligus",
  "version": "0.2.0",
  "command": "doctor",
  "ok": false,
  "exitCode": 1,
  "checks": [
    {
      "id": "relay.reachable",
      "title": "relay",
      "status": "fail",
      "detail": "https://relay.example.com: connection refused",
      "fix": "check the URL and this network; on the relay host: langstroth-relay check --url https://relay.example.com"
    }
  ],
  "next": ["langstroth-relay check --url https://relay.example.com"]
}
```

| Field | Rule |
|---|---|
| `schema` | Always `"ligus.checks.v1"` (a [profile](#profiles) keeps its own name). Consumers ignore fields they don't know; a new field never renames an existing one. |
| `tool`, `command` | The program and the subcommand that produced the report. |
| `version` | Optional: the tool's version. |
| `ok` | `true` when no check has `status: "fail"`. Warnings are allowed. |
| `exitCode` | The code the process exits with. |
| `checks[].id` | A stable, dotted machine key (`relay.reachable`, `sandbox.tier`). Scripts match on it; it never changes meaning. |
| `checks[].title` | Optional: a short label for the text form. |
| `checks[].status` | `ok`, `warn`, `fail` or `skip`. |
| `checks[].detail` | One line: what was found. |
| `checks[].fix` | Optional, one line: how to fix it, ideally a command. Every `fail` has one. |
| `checks[].notes` | Optional: more lines of what was found (a sandbox's caveats, the turn test's findings, the per-runtime policy). |
| `next[]` | Commands to run next, in order. Empty when there is nothing to do. |

### The text form {#text}

One line per check (status, title, what was found), an indented `fix:` line
under anything that isn't `ok`, then a summary:

```text
ok    secret store    keychain
warn  workspace       0b6c…: not synced or not a member yet
      fix: ligus sync
fail  relay           https://relay.example.com: connection refused
      fix: check the URL and this network; on the relay host: langstroth-relay check --url https://relay.example.com
1 failure, 1 warning · next: ligus sync ; langstroth-relay check --url https://relay.example.com
```

`ligus doctor` keeps two things from its earlier output because scripts read
them: its first two lines are `agent sandbox: <tier>` and the tier's summary,
and the turn test's findings are `turn test: …` lines.

### Each doctor names the next one down {#next-doctor}

A check that fails because of another component says which tool checks that
component. `ligus doctor`'s relay check names
`langstroth-relay check --url <relay>`, which you run on the relay host; the
deploy scripts name `ligus doctor` for the sandbox.

## Exit codes {#exit-codes}

One table for every tool. `ligus help exit-codes`, `ligus-k8s help
exit-codes`, `ligus-local help exit-codes` and `langstroth-relay help
exit-codes` print it.

| Code | Meaning | What a person or an agent does |
|---|---|---|
| 0 | done (warnings allowed) | continue |
| 1 | failed: the message or `checks[].fix` says why | read it, fix, retry |
| 2 | usage error | fix the command line |
| 3 | a precondition you can fix is missing (a tool, a file, a sign-in, the agent sandbox) | fix it and rerun |
| 4 | **a person must decide or act** (a passkey, an admission, exposure, deleting owned data) | stop and ask |
| 5 | not found (a workspace, chat, proposal, run) | check the id |
| 6 | refused by workspace rules (a role, lockdown, agent access) | ask an Admin; don't retry |
| 7 | offline, the relay unreachable, or timed out | retry with backoff |
| 8+ | command-specific, listed in that command's help | |

Which codes each tool uses: `ligus` all of 0 to 7 (4 only from `ligus setup`'s person steps; see the
[CLI reference](../../ligus/reference/cli.md#exit-codes)); the deploy scripts
0 to 4 (an unreachable cluster is 3: check the context); `langstroth-relay`
0 to 2. Every command's `--help` ends its EXIT CODES with a pointer to the
table.

A check command exits 1 when a check fails, whatever the check. Before 0.2,
`ligus doctor`'s preflights exited 3, 4 or 5 and `ligus gateway-check` 6;
`--legacy-exit-codes` keeps those codes for one release.

## Errors {#errors}

Every tool prints an error the same way on stderr: what happened on the first
line, after the tool's name, then why and how to fix it when it knows:

```text
ligus: sync failed
  why: error sending request for url (https://relay.example.com/v1/...)
  fix: check the network and the relay URL; ligus doctor checks the relay
```

A usage error's fix names the help to read, for example ``fix: see `ligus tail --help` ``. With
`--json` (or `-o json`), the error is also the one JSON document on stdout:

```json
{"schema": "ligus.error.v1", "tool": "ligus", "version": "0.2.0", "command": "sync",
 "exitCode": 7,
 "error": {"code": "relay_unreachable", "message": "sync failed",
           "why": "error sending request for url (...)",
           "fix": "check the network and the relay URL; ligus doctor checks the relay"}}
```

`error.code` is a stable machine code: `usage`, `not_found`, `refused`,
`relay_unreachable`, `relay_busy`, `relay_token_rejected`, `unreachable` or
`error`. `langstroth-relay` prints the same document. The deploy scripts keep
their profile: a usage error is a report with one failed `usage` check (its
`fix` names the help) and `"error": "<message>"`.

## Common flags {#flags}

| Flag | `ligus` | `ligus-local`, `ligus-k8s` | `langstroth-relay` |
|---|---|---|---|
| `--flag=value` | every flag | every flag | every flag |
| `--json` | read, status and check commands | every command | `check` |
| `-o json\|text` | every command (`-o json` is `--json`; `-o jsonl` is `--jsonl`) | every command | `check` |
| `-q`, `--quiet` | `ligus doctor`: only the checks that need attention | only the warn and FAIL checks | |
| `--color auto\|always\|never` | `ligus doctor`'s text | the `[FAIL]` and `[WARN]` marks on stderr | |
| `--dry-run` | `add-runtime`, `rm-runtime`, `add-agent`, `set-agent-host`, `setup <flow>` | `ligus-k8s install` | |
| `--wait`, `--timeout` | `ligus send` (wait for an answer; `--timeout` alone also waits, default 5m; no answer in time exits 7) | `ligus-k8s install` (`--timeout`) | |
| `--force` | | `ligus-local configure` (rewrite the env file) | |

Colour is only ever on a terminal unless asked for: `NO_COLOR` turns it off,
`CLICOLOR_FORCE` turns it on for a pipe, and `--color` overrides both. JSON is
never coloured. None of the tools ask for a confirmation, so there is no
`--yes`; a step that needs a person (a passkey, an approval) is never one a
flag can take. Commands name things by id (a chat id, a host id), never by
their position in a list.

## Help {#help}

Every tool's help has the same sections in the same order: **NAME** (with
what it does), **USAGE**, **COMMANDS** or **FLAGS**, **EXAMPLES**,
**ENVIRONMENT**, **EXIT CODES** and **SEE ALSO**. SEE ALSO names the same
thing in the other tools: `ligus doctor` points to `langstroth-relay check`
and the deploy scripts' preflights, `ligus admit request` to the desktop's
**workspace menu > Admit a box**, the scripts to `ligus help <topic>`.

| | One command | Everything | Machine-readable |
|---|---|---|---|
| `ligus` | `ligus <command> --help`, `ligus help <command>` | `ligus help` | `ligus help --json` (`ligus.help.v1`) |
| `langstroth-relay` | `langstroth-relay <command> --help` | `langstroth-relay help` | `langstroth-relay help --json` (`ligus.help.v1`) |
| `ligus-local`, `ligus-k8s` | `<tool> help <command>`, `<tool> <command> --help` | `<tool> --help` | |

## Profiles {#profiles}

A profile is an older report with the same fields that keeps its schema
string, so nothing that already parses it breaks.

**`ligus.agent-deploy.v1`** (`ligus-local` and `ligus-k8s --json`) conforms to
`ligus.checks.v1`: the same `tool`, `command`, `ok`, `exitCode`, `checks[]`
(`id`, `status`, `detail`, `fix`) and `next[]`. Two spellings differ: a passed
check says `pass` (read it as `ok`), and `info` is a finding that is neither
good nor bad (read it as `ok`). It has no `title` or `version`, and some
commands add context fields of their own (`namespace`, `release`, `dataDir`).
A usage error adds `"error": "<message>"`.

## Who produces what {#producers}

| Tool | Command | Schema |
|---|---|---|
| `ligus` | `ligus doctor --json` | `ligus.checks.v1` |
| `ligus` | `ligus setup [<flow>] --json` | `ligus.checks.v1` (a flow's report adds `flow`, `done`, `dryRun` and `results`) |
| `langstroth-relay` | `langstroth-relay check --json` | `ligus.checks.v1` (relay 0.3.2 printed `name` instead of `id`; releases after it print both for one release) |
| `deploy/local/ligus-local` | every command with `--json` | `ligus.agent-deploy.v1` profile |
| `deploy/kubernetes/ligus-k8s` | every command with `--json` | `ligus.agent-deploy.v1` profile |

`ligus status --json` is a state view, not a check report: it describes this
device in one document and has its own fields.

## `ligus doctor`'s checks {#doctor}

`ligus doctor` runs every check by default. `--sandbox` or `--gateways`
narrows it to those checks; a preflight flag (`--require-confined`,
`--expect-tier`, `--turn-test`) narrows it to the sandbox, as before, for init
containers and CI.

| id | What it checks |
|---|---|
| `sandbox.tier` | the agent sandbox tier (`seatbelt`, `bwrap`, `landlock` or `unconfined`) and its caveats; a warning when there is none, a failure with `--require-confined` or `--turn-test` |
| `sandbox.expected-tier` | with `--expect-tier`: the tier is the one asked for |
| `process.dumpable` | Linux: other processes of this user can't read its memory |
| `sandbox.grants` | whether this host applies the workspace's signed sandbox grants, and how many there are |
| `sandbox.turn-test` | with `--turn-test`: one real headless agent turn ran confined |
| `sandbox.policy` | with `--sandbox`: what an agent turn may read and write here (in `notes`) |
| `secrets.backend` | the secret store in use; a warning when it fell back to plain files |
| `store.schema` | the version of `ligus.db`; a failure (exit 3) when a newer Ligus wrote it ([database version](../security/index.md#seeds-and-backups)) |
| `store.backups` | full copies of `ligus.db` next to it, which hold invite seeds: a warning listing their paths; delete one once you no longer need it |
| `workspace.joined` | the device has a workspace, is a member, and holds a local copy |
| `relay.reachable` | one request to the relay (10 seconds at most): reachable and the access token accepted |
| `sync.held` | events held aside until this device can judge them |
| `gateway.<name>.hosted`, `gateway.<name>.cli`, `gateways.unlisted` | this host's model gateways, as `ligus gateway-check` proves them; `gateways` is skipped when the host lists none |
