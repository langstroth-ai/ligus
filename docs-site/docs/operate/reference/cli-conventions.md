# CLI conventions: checks

Every Ligus command-line tool reports checks the same way: `ligus doctor`,
`langstroth-relay check`, and the deploy scripts `deploy/local/ligus-local`
and `deploy/kubernetes/ligus-k8s`. A person reads the text form; a script or a
coding agent reads one JSON document with `--json`. `ligus help checks` prints
this page in short.

## The `ligus.checks.v1` report {#checks-v1}

With `--json`, a check command prints exactly one JSON document on stdout
(progress and errors go to stderr):

```json
{
  "schema": "ligus.checks.v1",
  "tool": "ligus",
  "version": "0.1.1",
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
| `workspace.joined` | the device has a workspace, is a member, and holds a local copy |
| `relay.reachable` | one request to the relay (10 seconds at most): reachable and the access token accepted |
| `sync.held` | events held aside until this device can judge them |
| `gateway.<name>.hosted`, `gateway.<name>.cli`, `gateways.unlisted` | this host's model gateways, as `ligus gateway-check` proves them; `gateways` is skipped when the host lists none |
