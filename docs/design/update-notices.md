# Update notices and the "newer peers" warning (design note)

Status: design only, not implemented. Target: `ligus` CLI 0.2 and `lt` 0.2
(`langstroth-ai/ligus-term`), one shared helper. Ligus is short for
*ligustica*, the Italian honeybee.

Neither CLI tells you a newer release exists, and neither tells you when a
newer peer is writing events this build can't read. Decided by the owner:
one check a day, opt-out, on a terminal only, reading public release metadata
only, with nothing that identifies you or your workspace. The protocol-skew
warning is separate and needs no network at all.

## 1. What exists today (code-verified)

- **Desktop app:** `check_for_app_update` (`app/src/lib.rs:12086-12107`)
  fetches `https://docs.langstroth.ai/version.json` (fallback
  `docs.ligus.ai`), capped at 64 KiB and 8 s (`:12008-12014`), compares a
  strictly parsed `vX.Y.Z` with the build's `LIGUS_RELEASE_TAG` (`dev` never
  prompts, `:12043-12046`), and only links to
  `https://github.com/langstroth-ai/ligus/releases…` (`:12068-12079`). The
  Tauri updater uses `…/releases/latest/download/latest.json`
  (`app/tauri.conf.json:40-44`). `docs-site/docs/version.json` is bumped by
  hand and today still says `v0.1.0` although `v0.1.1` is Latest.
- **`ligus` CLI and `lt`: no update check.** Versions: `ligus --version`
  (`crates/ligus-cli/src/main.rs:1900-1902`), `ligus_core::VERSION`; lt
  `LONG_VERSION` (`ligus-term/src/cli.rs:12-16`).
- **Install methods:** Homebrew formulae `ligus` and `lt` in
  `langstroth-ai/homebrew-ligus`; `install.sh` to `$HOME/.local/bin`
  (`LIGUS_INSTALL_DIR`); direct downloads; `cargo install`. Neither binary
  records how it was installed.
- **Releases:** `https://github.com/<org>/<repo>/releases/latest` answers
  `302` with `location: …/releases/tag/vX.Y.Z` for the release promoted to
  Latest (checked: `ligus` → `v0.1.1`, `ligus-term` → `v0.1.2`). New releases
  are published as prereleases and promoted by a person, so "Latest" is
  exactly the release we want people on.
- **No protocol version exists.** Events carry no schema version; an unknown
  `kind` folds to `SessionEvent::Unknown` (`crates/ligus-core/src/events.rs:570-577`,
  projection no-op at `:1249-1250`), a known kind this build can't parse is
  inert too (`envelope_wire.rs:206-208`), and the bytes are kept
  (`event_store.rs` test `ingest_stores_the_received_bytes`). **Nothing counts
  them.** `lt` pins `ligus-core`/`ligus-runtime` to tag `v0.1.0`
  (`ligus-term/Cargo.toml:37-38`) while the desktop is 0.1.1, so skew is
  real today.
- **lt flags and env:** `--plain`, `--json` (`src/cli.rs:33-43`); TUI only
  when stdin and stdout are terminals (`src/main.rs:98`); env vars documented
  in `src/help.rs` `ENV`; config `deny_unknown_fields` (`src/config.rs:17-24`).
  Status bar right zone: `sync_label` (`src/ui/mod.rs:209-221`).

## 2. Update notices

### 2.1 When a check may run

All of these must hold:

- stdout **and** stderr are terminals;
- not `--plain`, not `--json`, not a one-shot `lt COMMAND`, not `ligus …
  --json`, not `ligus serve` / `ligus worker` (daemons never check);
- `CI` is unset or empty;
- `LT_NO_UPDATE_CHECK` / `LIGUS_NO_UPDATE_CHECK` is not set to a true value
  (`1|true|on|yes`; any other non-empty value is a usage error, exit 2, as
  `LT_NO_BROWSER`);
- the build is a release build (`LIGUS_RELEASE_TAG` / lt's release tag baked
  in; `dev` and `cargo install --path .` never check);
- the last attempt was more than 24 hours ago (state file below), successful
  or not.

### 2.2 The request

One request, in the background, never blocking the command:

```
HEAD https://github.com/langstroth-ai/ligus/releases/latest        (ligus)
HEAD https://github.com/langstroth-ai/ligus-term/releases/latest   (lt)
User-Agent: ligus-update-check        (same string for both tools, no version)
```

- No redirect is followed. The answer must be `302` with a `location`
  exactly matching `^https://github\.com/langstroth-ai/(ligus|ligus-term)/releases/tag/v(\d{1,4})\.(\d{1,4})\.(\d{1,4})$`;
  anything else is ignored silently.
- No query string, no cookies, no auth header (a `GITHUB_TOKEN` in the
  environment is never sent), no body. The standard proxy variables are
  honoured; nothing else from the environment is used.
- 5-second total timeout. No retry until the next day.
- What GitHub can learn: the client IP, the time, and that some Ligus CLI on
  that IP checks daily. Not which workspace, account, device, version or
  install method.

### 2.3 State

`update-check.json` in the tool's own data dir (`lt`: `LT_DATA_DIR` or
`~/.ligus-term`; `ligus`: `LIGUS_DATA_DIR` or `~/.ligus`), never the desktop
app's: `{ "checkedAt": "…", "latest": "0.1.3" }`, mode 0600, written
atomically. A read-only or missing data dir means no check (and no error).

### 2.4 The notice

Shown when `latest` (from state, possibly from an earlier run) is strictly
newer than this build by semver, ignoring prereleases:

- **`ligus`:** one dim line on stderr after the command's own output, only
  if the command succeeded:
  `ligus 0.1.3 is out (you have 0.1.1) · brew upgrade ligus`
- **`lt`:** the status bar's right zone shows `↑ 0.1.3` (dim) and Settings
  shows the full line. On exit, one dim line as for `ligus`.
- The upgrade hint comes from `std::env::current_exe()` canonicalised:
  a path under a Homebrew `Cellar` → `brew upgrade ligus` / `brew upgrade lt`;
  `~/.cargo/bin` → the `cargo install --locked --git … --tag vX.Y.Z` line;
  anything else → `https://github.com/langstroth-ai/<repo>/releases/latest`.
  lt never runs an upgrade.
- The version printed is the one parsed by the regex above, re-formatted from
  integers, so no byte from the network reaches the terminal (no escape
  injection).

### 2.5 Shared helper

A small module in `ligus-core` (no network types) does the policy (2.1),
parsing (2.2), state (2.3) and formatting (2.4) as pure functions; each tool
does the HTTP call with its existing client (`ligus_forge::http_client_builder`
for `ligus`; lt through `ligus-runtime`). lt picks it up when it moves its
`ligus-core` pin.

## 3. "Newer peers" (protocol skew)

No network involved: it reads the local event store.

- When folding, count envelopes that decode to `Unknown`, or that are a known
  kind with an unparseable payload, per workspace. Both already land in the
  event store with their bytes, and the store's `kind` column holds
  `kind_str()`, which is `"unknown"` for both
  (`crates/ligus-runtime/src/event_store.rs:545, 607, 811, 920`). So the count
  is `SELECT count(*) FROM events WHERE workspace_id = ? AND kind = 'unknown'`,
  computed at open and updated on ingest; the wire kinds for the Settings list
  are read from those rows' `envelope_json` only when Settings opens.
- **lt:** while the count is above zero, the status bar's right zone shows,
  in the warn hue: `⚠ newer peers · update lt to read 3 events`. It replaces
  the update arrow (not both). Settings lists the kinds by name.
- **`ligus status`:** a line `unreadable events : 3 (update ligus to read
  them)`; in `--json`, `"unreadableEvents": 3`.
- **Desktop:** the status bar's sync item shows the same count in its detail
  popover. Not a banner.
- The message never claims what the events are; kinds are shown as their
  wire ids, sanitised to `[A-Za-z0-9_]`, 40 characters at most.

## 4. Tests (executable)

| # | Test | Kind |
|---|---|---|
| T1 | Policy truth table: every combination of TTY flags, `--plain`/`--json`, `CI`, opt-out values, dev build, last check age → check or not. | unit (table) |
| T2 | Only `302` with an exactly matching `location` yields a version; `200`, other hosts, `…/tag/v1.2.3-rc1`, extra path segments, a 10 KB header and ANSI escapes in the header all yield nothing. | unit + fuzz |
| T3 | The request carries exactly `User-Agent: ligus-update-check`, no auth even with `GITHUB_TOKEN` set, no query string (against a local test server). | integration |
| T4 | A slow server (10 s) never delays the command's exit beyond its normal time. | integration with timing |
| T5 | Install-method hint for a Cellar path, a cargo path and `~/.local/bin`. | unit |
| T6 | State file is 0600, atomic, never in the desktop data dir; a read-only data dir disables the check. | unit |
| T7 | A store with one unknown kind and one unparseable known kind gives count 2, and Settings names both wire kinds; lt's status bar renders the warning at 80 columns. | unit + `lt` screen test |
| T8 | `ligus serve` and `ligus worker` never make the request (no listener hit during a run). | integration |

## 5. Adversarial assessment

Stance: the adversarial reviewer, light security lens. The first pass looked
at a draft that called the GitHub REST API (`/repos/…/releases/latest`)
and printed the release `name`.

### First pass: Revise

1. **Terminal injection** (High, medium). Printing a release `name` or tag
   from the network lets anyone who controls that response (or a malicious
   proxy) write escape sequences to the terminal. *Revision: integer-only
   re-formatting (2.4), test T2.*
2. **Token leak** (Medium, medium). HTTP clients that pick up `GITHUB_TOKEN`
   for api.github.com would send it. *Revision: HEAD on github.com, no auth,
   test T3.*
3. **API rate limits** (Low, high). Unauthenticated API calls are limited
   per IP; behind a shared NAT the check fails for everyone. *Revision: the
   redirect, which isn't an API call.*
4. **Stale manifest** (Low, high). Reusing `version.json` would inherit its
   hand-bump drift (it still says `v0.1.0`). *Revision: GitHub's Latest, which
   is what a person promotes.*
5. **Skew is silent** (Medium, high). Without a count, a member on an old lt
   sees fewer events and doesn't know. *Revision: section 3, test T7.*

### Second pass

#### Decision

**Approve with risks.**

#### Claimed invariants

- U1. At most one request per tool per 24 hours, only on an interactive
  terminal, never from a daemon or CI.
- U2. The request carries nothing that identifies the person, workspace,
  device, version or install method.
- U3. Nothing from the network reaches the terminal except three integers.
- U4. The skew warning needs no network and never names event contents.

#### Hidden assumptions

- GitHub keeps answering `releases/latest` with a `302` to `/tag/…`. If it
  changes, the check goes quiet (fail-quiet, not fail-loud).
- The IP address is an identifier we accept (the owner's decision allows a
  request at all).

#### Counterexamples considered after revision

| Claim | State | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| U1 | Many short `ligus` invocations from scripts on a TTY | one a day | one a day (state file) | Low | High |
| U2 | Two tools on one machine | unlinkable | same IP, same UA: GitHub learns "both tools here" | Low | High |
| U4 | A peer's event is malformed, not newer | not "newer peers" | counted as unreadable; wording says "update" | Low | Medium |

#### Required tests

T1-T8.

#### Simpler alternatives

No network at all: show only the skew warning, which already covers the case
that matters for correctness (can't read newer events). The owner chose to
allow the daily check; this design keeps it removable without touching the
skew warning.

#### Residual risks

- GitHub sees the IP and a daily timing pattern.
- "Unreadable" counts include malformed events from a buggy peer, not only
  newer ones.
</content>
</invoke>
