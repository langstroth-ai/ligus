//! `ligus help`: the built-in reference and guides, so a person or an agent
//! can find and use every capability from the CLI alone.
//!
//! - `ligus help` lists the commands and the topics;
//! - `ligus <command> [<subcommand>] --help` (or `-h`), and
//!   `ligus help <command> [<subcommand>]`, print one command's reference:
//!   purpose, synopsis, flags with defaults, environment, examples, exit
//!   codes and see also;
//! - `ligus help <topic>` prints a task guide (the exact commands in order).
//!   A name that is both a topic and a command (`agents`, `repos`,
//!   `remote`, `worker`) prints the guide, then the command reference.
//!
//! Help goes to stdout and exits 0; an unknown command or topic exits 2.
//! Tests in this file check that every command the dispatcher in `main.rs`
//! (and its subcommand dispatchers) handles has an entry here with every
//! section, and that every `LIGUS_*` variable the CLI reads is in
//! `ligus help env`.

use std::fmt::Write as _;

/// One flag: how it is written, its default, what it does.
pub struct Flag {
    pub flag: &'static str,
    pub default: &'static str,
    pub about: &'static str,
}

const fn f(flag: &'static str, default: &'static str, about: &'static str) -> Flag {
    Flag { flag, default, about }
}

/// One command or subcommand's reference.
pub struct Cmd {
    /// As typed after `ligus`: `remote enable`.
    pub name: &'static str,
    /// The section `ligus help` lists it under.
    pub group: &'static str,
    /// One line for the command list.
    pub summary: &'static str,
    pub purpose: &'static str,
    pub synopsis: &'static [&'static str],
    pub flags: &'static [Flag],
    /// Variables read by this command besides the common ones below.
    pub env: &'static [&'static str],
    /// Opens the data dir (adds `LIGUS_DATA_DIR`, `LIGUS_SECRET_BACKEND`).
    pub data: bool,
    /// Uses the workspace connection (adds the relay and workspace variables).
    pub conn: bool,
    pub examples: &'static [&'static str],
    /// Exit codes besides 0, 1 and 2.
    pub exit: &'static [(i32, &'static str)],
    pub see_also: &'static [&'static str],
}

const D: Cmd = Cmd {
    name: "",
    group: "",
    summary: "",
    purpose: "",
    synopsis: &[],
    flags: &[],
    env: &[],
    data: true,
    conn: false,
    examples: &[],
    exit: &[],
    see_also: &[],
};

/// Exit codes every command shares.
pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

/// A wrong command line: exits [`EXIT_USAGE`]. Any error whose message starts
/// with `usage:` counts too (see [`is_usage`]).
#[derive(Debug)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// Whether `e` is a usage error (exit 2).
pub fn is_usage(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    e.downcast_ref::<UsageError>().is_some() || msg.starts_with("usage:") || msg.starts_with("unknown `ligus ")
}

/// The help to read after a usage error on `args`: the deepest command they
/// name, else the command list.
pub fn hint(args: &[&str]) -> String {
    match cmd_of(&words(args)) {
        Some(c) => format!("see `ligus {} --help`", c.name),
        None => "see `ligus help`".into(),
    }
}

/// Whether `name` is a top-level command (`ligus help` lists them).
pub fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|c| c.name == name)
}

/// A usage error naming the help to read.
pub fn usage_error(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UsageError(msg.into()))
}

const G_START: &str = "Getting started";
const G_CHAT: &str = "Chats and messages";
const G_AGENTS: &str = "Runtimes, agents and hosts";
const G_BOX: &str = "Boxes: serve, worker, admission";
const G_PHONE: &str = "Phone and remote access";
const G_REPOS: &str = "Repositories and code hosts";
const G_DATA: &str = "Memory, facts and dashboards";
const G_CHECK: &str = "Checks and diagnostics";

const GROUPS: &[&str] = &[G_START, G_CHAT, G_AGENTS, G_BOX, G_PHONE, G_REPOS, G_DATA, G_CHECK];

const JSON: Flag = f("--json", "off", "print one JSON document on stdout instead of text (also: -o json)");
const DRY_RUN: Flag = f("--dry-run", "off", "check the arguments and print what would change; sign and write nothing");

pub const COMMANDS: &[Cmd] = &[
    // ── Getting started ────────────────────────────────────────────────
    Cmd {
        name: "help",
        group: G_START,
        summary: "this help; `ligus help <command>` or `ligus help <topic>`",
        purpose: "Print the command list and the guides (no argument), one command's reference, or a task guide. \
                  It never opens the data dir.",
        synopsis: &["ligus help", "ligus help <command> [<subcommand>]", "ligus help <topic>", "ligus <command> [<subcommand>] --help | -h"],
        data: false,
        examples: &["ligus help", "ligus help box", "ligus help remote enable"],
        see_also: &["ligus help agents-guide", "ligus help env"],
        ..D
    },
    Cmd {
        name: "version",
        group: G_START,
        summary: "print the version (also --version, -V)",
        purpose: "Print `ligus <version>`. It never opens the data dir.",
        synopsis: &["ligus --version | -V | version"],
        data: false,
        examples: &["ligus --version"],
        see_also: &["ligus status"],
        ..D
    },
    Cmd {
        name: "status",
        group: G_START,
        summary: "one-screen summary: data dir, identity, workspace, relay, admission, serve, sandbox",
        purpose: "Summarise this data dir without changing it: the identity (and whether it is a box), the workspace and \
                  its relay, whether an access token, a workspace key and a local copy of the workspace are present, \
                  whether this identity is a member, a kept admission, a link to your devices still waiting for the \
                  desktop's code (linking: started, approve by, expires), the serve and remote-access state, the agent \
                  sandbox tier, and the next step. Never creates an identity; reads only.",
        synopsis: &["ligus status [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus status", "LIGUS_DATA_DIR=~/.ligus-serve ligus status --json"],
        see_also: &["ligus whoami", "ligus remote status", "ligus doctor", "ligus help troubleshooting", "deploy/local/ligus-local verify (a box this machine runs)"],
        ..D
    },
    Cmd {
        name: "whoami",
        group: G_START,
        summary: "this client's account and device ids, name, data dir and room",
        purpose: "Print this data dir's identity: account id, device id, display name, data dir and room. Creates the \
                  identity on first use.",
        synopsis: &["ligus whoami [--json]"],
        flags: &[JSON],
        env: &["LIGUS_WORKSPACE", "LIGUS_SERVE_LABEL"],
        examples: &["ligus whoami", "ligus whoami --json"],
        see_also: &["ligus status"],
        ..D
    },
    Cmd {
        name: "enroll",
        group: G_START,
        summary: "a person's own CLI device: save a workspace connection from an invite",
        purpose: "For a person's own CLI device (never a serve or worker box: a box joins with `ligus admit`). An invite makes \
                  a NEW member: if you are already in the workspace on your desktop, link this computer instead \
                  (`ligus devices request`). Decode a workspace invite (`hivews1:…`: relay URL, room, end-to-end key) and save it: the relay and \
                  room to <data dir>/config.toml, the key and the optional relay access token to the secret store. \
                  An invite carries the workspace key: pipe it or pass a file; on the command line it works with a \
                  warning. Refuses an invite whose workspace has no usable relay URL.",
        synopsis: &["ligus enroll - [--token - | @FILE]", "ligus enroll --file PATH [--token - | @FILE]", "ligus enroll <hivews1:invite> [--token …]"],
        flags: &[
            f("-", "", "read the invite from stdin"),
            f("--file PATH", "none", "read the invite from a file"),
            f("--token - | @FILE", "none", "relay access token, from stdin or an owner-only file (a literal value works with a warning)"),
        ],
        examples: &["pbpaste | ligus enroll -", "ligus enroll --file ./invite --token @./relay-token", "ligus enroll --file ./invite && ligus sync"],
        see_also: &["ligus devices request (your own second computer)", "ligus sync", "ligus help quickstart", "ligus admit (a box)", "ligus help security"],
        ..D
    },
    Cmd {
        name: "devices",
        group: G_START,
        summary: "link this computer as one of your own devices (not a new member)",
        purpose: "Add a computer: make this computer one of YOUR devices in a workspace you are already in, so what it \
                  does is yours and not a separate member's (an invite always makes a new member). This computer prints \
                  a link request (no secret); you approve it on your desktop (Settings > Profile > My devices > Add a \
                  computer) with your passkey; the desktop shows a grant sealed to this computer (the workspace key \
                  inside, readable only here); you redeem it here. The computer then acts as you without your account \
                  key: votes ask for your passkey, and account changes (devices, phones, passkeys, keys) stay on the \
                  desktop. One person per data dir. Revoke it from My devices.",
        synopsis: &[
            "ligus devices request [--label NAME] [--new-device] [--copy]",
            "ligus devices redeem [<ligusgrant1:code> | - | --code-file PATH] [--approver-fingerprint FP]",
            "ligus devices approve <liguslink1:request> | --code-file PATH --fingerprint FP --recovery-words-file PATH [--out PATH]",
            "ligus devices list",
            "ligus devices rename <device-id> <name>",
        ],
        flags: &[JSON],
        conn: true,
        examples: &[
            "ligus devices request | pbcopy",
            "ligus devices redeem",
            "ligus devices list --json",
        ],
        exit: &[
            (3, "no usable relay URL"),
            (4, "a person must act: approve on the desktop, compare a fingerprint, confirm the approving desktop"),
            (6, "refused: a box, a computer already someone else's, lockdown, revoked"),
            (7, "the relay is unreachable, or the desktop's approval hasn't synced yet (retry)"),
        ],
        see_also: &["ligus devices request", "ligus devices redeem", "ligus devices approve", "ligus devices list", "desktop: Settings > Profile > My devices > Add a computer"],
        ..D
    },
    Cmd {
        name: "devices request",
        group: G_START,
        summary: "print this computer's link request for your desktop",
        purpose: "Print this computer's link request (`liguslink1:…`, no secret) and remember it as pending (15 \
                  minutes to approve, single use). Only the request goes to stdout, so `ligus devices request | pbcopy` \
                  copies exactly it; the fingerprint, the time to approve by and the next step go to stderr. --copy \
                  copies it to the clipboard instead (pbcopy, wl-copy, xclip, xsel, clip.exe; printed when none works). \
                  --json prints one report whose result has code, fingerprint, label, expiresAt (unix ms: approve by \
                  then) and newDevice. Expired requests are cleaned up as you go. With --to (the approval address your \
                  desktop shows under My devices) the request also goes to your desktop's Inbox: both screens then show \
                  the same 8-symbol code, you confirm it here, your desktop confirms with your passkey, and this computer \
                  links after checking the approval on the workspace log and asking \"Link this computer as <you>?\". The \
                  paste text and fingerprint are printed straight away for an older desktop. An install that was already connected (it may have written as its own \
                  member) asks with a new device key, switched to only when you redeem, and adds a proof that it was that \
                  member, so the desktop can offer to remove the duplicate. A box can't: it joins with `ligus admit`.",
        synopsis: &[
            "ligus devices request [--label NAME] [--new-device] [--copy] [--json]",
            "ligus devices request --to ligusaddr1:… [--label NAME] [--wait 15m] [--new-device] [--json]",
        ],
        flags: &[
            f("--label NAME", "ligus on <host name>", "the label your desktop shows"),
            f("--new-device", "off (on when this data dir was connected)", "ask with a new device key"),
            f("--copy", "off", "copy the request to the clipboard instead of printing it"),
            f("--to ligusaddr1:…", "none (print the request to paste)", "send the request to the workspace's approval address (code match): it reaches your desktop's Inbox"),
            f("--wait DUR", "15m", "with --to: how long to wait for your desktop (90s, 5m, 1h)"),
            f("--insecure-relay", "off", "with --to: allow an http:// relay that isn't loopback (tests)"),
            JSON,
        ],
        examples: &[
            "ligus devices request --copy",
            "ligus devices request --label work-laptop | pbcopy",
            "ligus devices request --to ligusaddr1:… --label work-laptop",
        ],
        exit: &[
            (3, "with --to: the relay has no approval mailbox (paste the request instead: ligus devices request)"),
            (4, "with --to: no terminal (a person confirms the code and who they link as here), or nobody approved within --wait"),
            (6, "this data dir is a box, or with --to the relay refused the access token"),
            (7, "with --to: the relay is unreachable, rate-limited or its mailbox is full"),
        ],
        see_also: &["ligus devices redeem", "desktop: Settings > Profile > My devices > Add a computer"],
        ..D
    },
    Cmd {
        name: "devices rename",
        group: G_START,
        summary: "give one of your devices (or this one) a name every member sees",
        purpose: "Set the synced name of one of your devices in this workspace: this computer, or another device of \
                  yours (from a computer that holds your account key; a linked computer renames only itself). Every \
                  member's My devices and People show it. An empty name clears it. `ligus devices list --json` lists the \
                  device ids.",
        synopsis: &["ligus devices rename <device-id> <name> [--json]", "ligus devices rename this <name> [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus devices rename this \"Ana's build box\"", "ligus devices rename 3c1e0f2a-... \"Ana's iPhone\""],
        exit: &[(2, "no device id or name"), (6, "refused: not one of your devices, or a linked computer renaming another device")],
        see_also: &["ligus devices list", "desktop: Settings > Profile > My devices > Rename"],
        ..D
    },
    Cmd {
        name: "profile",
        group: G_START,
        summary: "your name and picture, as every member sees them",
        purpose: "Your profile is the one place your name and picture come from in a workspace: every member's \
                  People, chats and devices show it, in past content too. Set it with `ligus profile set` (or on your \
                  desktop: Settings > Profile).",
        synopsis: &["ligus profile set [--name NAME] [--avatar FILE | --clear-avatar] [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus profile set --name \"Ana Ruiz\""],
        exit: &[(2, "unknown subcommand")],
        see_also: &["ligus profile set"],
        ..D
    },
    Cmd {
        name: "profile set",
        group: G_START,
        summary: "set your name and picture in every workspace you're in",
        purpose: "Publish your name and/or picture as your profile in every workspace this data dir holds where you \
                  are a member: every member sees the new name, in past content too. Only a computer that holds your \
                  account key can (a linked computer, a box or a phone can't: change it on your desktop). A picture is a \
                  PNG, JPEG or WebP file of 16 KB at most. At most one change a minute and 20 a day per workspace.",
        synopsis: &["ligus profile set [--name NAME] [--avatar FILE | --clear-avatar] [--json]"],
        flags: &[
            f("--name NAME", "unchanged", "your display name"),
            f("--avatar FILE", "unchanged", "a PNG, JPEG or WebP picture, 16 KB at most"),
            f("--clear-avatar", "off", "remove your picture"),
            JSON,
        ],
        conn: true,
        examples: &["ligus profile set --name \"Ana Ruiz\"", "ligus profile set --avatar ./me.webp"],
        exit: &[(1, "the throttle: try again in a minute"), (2, "nothing to set, or a picture that isn't PNG, JPEG or WebP, or is over 16 KB"), (6, "this computer acts for you without your account key")],
        see_also: &["desktop: Settings > Profile"],
        ..D
    },
    Cmd {
        name: "devices redeem",
        group: G_START,
        summary: "link this computer with the grant your desktop showed",
        purpose: "Open the grant (`ligusgrant1:…`, sealed to this computer) that answers this computer's pending request, \
                  confirm who approved it (the person, the workspace and the approving desktop's fingerprint: \
                  --approver-fingerprint, or a yes at a terminal), save the connection (the workspace key to the secret \
                  store), pull the workspace, check your desktop's certificate for this computer is there, and act as you \
                  from then on. With no code at a terminal it asks for a hidden paste, and reads the clipboard when you \
                  just press Enter (pbpaste, wl-paste, xclip, xsel, PowerShell); whatever surrounds the code (a whole \
                  copied command, quotes, line breaks) is dropped, and the code is never echoed. Without \
                  --approver-fingerprint, a terminal asks: Approved by <person>'s desktop · fingerprint … · for \
                  <workspace>. Is that your desktop? [y/N] (y links it); anywhere else it exits 4. \
                  Publishes this computer's key-agreement key so later key rotations reach it. The relay \
                  access token is never in the grant: on a gated relay, sign in here (ligus auth login).",
        synopsis: &["ligus devices redeem [<ligusgrant1:code> | - | --code-file PATH] [--approver-fingerprint FP] [--json]"],
        flags: &[
            f("-", "", "read the grant from stdin"),
            f("--code-file PATH", "none", "read the grant from a file"),
            f("--approver-fingerprint FP", "none (asks at a terminal)", "the approving desktop's fingerprint, as its sheet shows"),
            JSON,
        ],
        conn: true,
        examples: &[
            "ligus devices redeem",
            "pbpaste | ligus devices redeem - --approver-fingerprint 7F3A-91C0-77DE-0B14-5E62",
            "ligus devices redeem --code-file ./grant --approver-fingerprint 7F3A-91C0-77DE-0B14-5E62",
        ],
        exit: &[
            (4, "no code given away from a terminal, or confirm the approving desktop (pass --approver-fingerprint or run at a terminal)"),
            (6, "refused: already someone else's device, another workspace in this data dir, lockdown, revoked"),
            (7, "the relay is unreachable, or the desktop's approval hasn't synced yet (retry within 30 minutes)"),
        ],
        see_also: &["ligus devices request", "ligus devices list", "ligus auth login"],
        ..D
    },
    Cmd {
        name: "devices approve",
        group: G_START,
        summary: "approve another computer as yours, headless (prefer the desktop)",
        purpose: "A person's own step (prefer the desktop's My devices > Add a computer): certify the computer whose \
                  request is given as one of your devices in this workspace, confirmed with your recovery key, sync the \
                  certificate, and print the grant for that computer (sealed to it). The fingerprint check is required: \
                  --fingerprint, or a yes at a terminal. Never from a box or a linked computer (no account key).",
        synopsis: &["ligus devices approve <liguslink1:request> | --code-file PATH --fingerprint FP --recovery-words-file PATH [--out PATH] [--json]"],
        flags: &[
            f("--code-file PATH", "none", "read the request from a file"),
            f("--fingerprint FP", "none (asks at a terminal)", "the computer's fingerprint as its screen shows it"),
            f("--recovery-words-file PATH", "none", "your recovery key's 24 words (an owner-only file): your presence"),
            f("--out PATH", "stdout", "write the grant to an owner-only file"),
            JSON,
        ],
        conn: true,
        examples: &["ligus devices approve --code-file ./request --fingerprint 7F3A-91C0-77DE-0B14-5E62 --recovery-words-file ~/.ligus-recovery --out ./grant"],
        exit: &[(4, "compare the fingerprint, or confirm with your passkey or recovery key"), (6, "refused: not yours to add, lockdown, already here")],
        see_also: &["ligus devices request", "ligus recovery-key create", "desktop: Settings > Profile > My devices > Add a computer"],
        ..D
    },
    Cmd {
        name: "devices list",
        group: G_START,
        summary: "your devices in this workspace, and where this computer stands",
        purpose: "List the devices your account certified in this workspace (fingerprint, revoked, this device), whether \
                  this computer acts as you, as its own identity or is revoked, and pending link requests. Read-only.",
        synopsis: &["ligus devices list [--json]"],
        flags: &[JSON],
        examples: &["ligus devices list --json"],
        see_also: &["ligus devices request", "desktop: Settings > Profile > My devices"],
        ..D
    },
    Cmd {
        name: "join",
        group: G_START,
        summary: "redeem a relay invite code to self-enroll this identity",
        purpose: "Redeem a relay invite code, adding this identity to the configured workspace's relay roster at the \
                  invite's role. Needs a relay URL (from `ligus enroll` or LIGUS_RELAY_URL).",
        synopsis: &["ligus join <invite-code>"],
        conn: true,
        examples: &["ligus join 7Q4K-…", "ligus join 7Q4K-… && ligus sync"],
        see_also: &["ligus enroll", "ligus sync"],
        ..D
    },
    Cmd {
        name: "sync",
        group: G_START,
        summary: "push and pull once with the relay (--watch to repeat)",
        purpose: "One sync round: push every unpushed event, then pull and ingest, paging until caught up. Prints \
                  `pushed N, pulled M`; a relay refusal (quota, rate limit) is printed as `sync: …`.",
        synopsis: &["ligus sync [--watch]"],
        flags: &[f("--watch", "off", "repeat every 3 s until stopped")],
        conn: true,
        examples: &["ligus sync", "ligus sync --watch"],
        see_also: &["ligus enroll", "ligus status", "ligus help troubleshooting"],
        ..D
    },
    Cmd {
        name: "setup",
        group: G_START,
        summary: "guided setup: list the flows and what is done here",
        purpose: "List the seven guided setup flows (agent, tool, folder, box, phone, invite, relay) and what each has \
                  done on this device, from the same engine the desktop's Set up commands use. `ligus setup <flow> \
                  --help` shows a flow's flags. Every flow checks first, writes only when every check passed, is safe \
                  to run again, and stops with exit 4 at a step only a person can do. `ligus help setup` is the guide.",
        synopsis: &["ligus setup [--json]", "ligus setup <flow> [flags] [--recovery-words-file PATH] [--json] [--dry-run]"],
        flags: &[
            JSON,
            f("--apply", "off", "the earlier `ligus setup --apply`: now `ligus setup runtimes --apply`"),
            f("--recovery-words-file PATH", "none", "your recovery key's 24 words (an owner-only file): your presence for a step that needs your passkey"),
        ],
        examples: &["ligus setup", "ligus setup agent --harness claude-code --name builder", "ligus setup --json | jq '.checks[] | {id, status}'"],
        exit: &[(0, "listed (setup never fails a listing)")],
        see_also: &["ligus help setup", "ligus doctor", "desktop: ⌘K > Set up an agent (and the other Set up commands)"],
        ..D
    },
    Cmd {
        name: "setup agent",
        group: G_START,
        summary: "set up an agent on a coding CLI or model that is installed and signed in",
        purpose: "Check the harness is installed and signed in (never reading a token), check the agent sandbox, \
                  validate the name (one word; not a reserved mention name such as ligus or all; not another agent's or \
                  member's), pick the model, reuse a keyless workspace runtime with that harness and model or define one \
                  (Admin), create the agent (Contributor), and optionally send one test turn. A failure after the \
                  runtime was defined removes it again.",
        synopsis: &["ligus setup agent --harness H --name NAME [--model M] [--endpoint URL | --gateway NAME] [--local-servers] [--test-turn] [--recovery-words-file PATH] [--json] [--dry-run]"],
        flags: &[
            f("--harness H", "required", "claude-code, codex, copilot, antigravity, aider, pi, local (Ollama or OpenAI-compatible) or gateway"),
            f("--name NAME", "required", "the agent's @name"),
            f("--model M", "the harness's default", "the model (required for local)"),
            f("--endpoint URL", "http://localhost:11434", "local only: the model server (https://, or http:// on this machine)"),
            f("--gateway NAME", "none", "gateway only: a model gateway a host offers (ligus hosts)"),
            f("--test-turn", "off", "send one short test message and check the reply"),
            f("--local-servers", "off", "let the agent run local servers (agy's helper server; Linux with bubblewrap; not available on macOS yet; the Owner's)"),
            f("--recovery-words-file PATH", "none", "your recovery key's 24 words (an owner-only file): your presence when the workspace's rules need your passkey"),
            JSON,
            DRY_RUN,
        ],
        env: &["PATH", "LIGUS_RUNTIME_TEST_TIMEOUT_SECS"],
        examples: &[
            "ligus setup agent --harness claude-code --name builder --test-turn",
            "ligus setup agent --harness local --model llama3.2 --name local --dry-run",
            "ligus setup agent --harness antigravity --name gem --local-servers --json",
        ],
        exit: &[
            (2, "a missing or invalid flag (the check names it)"),
            (3, "the harness isn't installed or signed in, or no agent sandbox"),
            (4, "the workspace's rules need your passkey: do it in the desktop app, or pass --recovery-words-file"),
            (6, "your role can't add an agent or a workspace runtime"),
            (7, "a local model server didn't answer"),
        ],
        see_also: &["ligus setup", "ligus add-agent", "ligus runtimes", "ligus doctor --sandbox"],
        ..D
    },
    Cmd {
        name: "setup tool",
        group: G_START,
        summary: "connect an MCP server and choose which agents may reach it",
        purpose: "Validate the server (a command, or an https URL; no token in the URL or the arguments), start it once \
                  under the agent sandbox and list its tools before anything is saved, then save it with its reach in \
                  one write to <data dir>/mcp-servers.toml (owner-only; ligus worker and serve read it with \
                  LIGUS_MCP_CONFIG). A bearer token goes to this device's secret store only. Which agents may reach it \
                  is kept on this device (mcp-reach.json), never in a file a commit could change. Refused during \
                  workspace lockdown unless the lockdown allows the server.",
        synopsis: &["ligus setup tool --id ID (--command \"PROG ARGS\" | --kind url --url URL [--token - | @FILE]) [--reach all|AGENT,...] [--json] [--dry-run]"],
        flags: &[
            f("--id ID", "required", "the server's id: lowercase letters, digits and -"),
            f("--kind command|url", "command", "a command on this device or a URL"),
            f("--command \"PROG ARGS\"", "required for command", "the command line; never put a token in it"),
            f("--url URL", "required for url", "https://, or http:// on this machine"),
            f("--token - | @FILE", "none", "the bearer token, from stdin or an owner-only file (never on the command line)"),
            f("--reach all|AGENT,...", "all", "which agents may use it; the default agent gets only servers every agent may use"),
            JSON,
            DRY_RUN,
        ],
        env: &["LIGUS_MCP_CONFIG"],
        examples: &[
            "ligus setup tool --id notes --command \"npx -y @modelcontextprotocol/server-filesystem ~/notes\" --reach builder",
            "ligus setup tool --id linear --kind url --url https://mcp.linear.app/mcp --token @./linear-token",
        ],
        exit: &[(2, "an invalid flag, a token-looking argument or URL"), (3, "no agent sandbox on this host"), (6, "your role, or the workspace lockdown"), (7, "the server didn't answer")],
        see_also: &["ligus setup", "ligus help env", "ligus doctor --sandbox"],
        ..D
    },
    Cmd {
        name: "setup folder",
        group: G_START,
        summary: "bind a git checkout on this device to a workspace repository",
        purpose: "Check the folder is a main git checkout (not a linked worktree), not managed by another host here and \
                  not bound to another repository, before anything is written; then match the workspace repository by \
                  its remote (adding it, with its default branch, takes an Admin) and bind the folder (Contributor).",
        synopsis: &["ligus setup folder --path DIR [--name NAME] [--branch B] [--json] [--dry-run]", "ligus setup folder --scratch [--name NAME] [--json] [--dry-run]"],
        flags: &[
            f("--scratch", "off", "start with a scratch repository instead: an empty one this host creates, added to the workspace flagged scratch (Admin) and bound here"),
            f("--path DIR", "required without --scratch", "the checkout"),
            f("--name NAME", "the repository's name", "the display name when the repository is added"),
            f("--branch B", "origin's HEAD, else main or master", "the default branch when the repository is added"),
            JSON,
            DRY_RUN,
        ],
        examples: &["ligus setup folder --path ~/src/app", "ligus setup folder --path . --dry-run --json"],
        exit: &[(2, "not a checkout, a linked worktree, managed elsewhere or bound to another repository"), (3, "git isn't installed"), (6, "adding a repository needs an Admin; binding a Contributor")],
        see_also: &["ligus setup", "ligus repos", "ligus help repos"],
        ..D
    },
    Cmd {
        name: "setup box",
        group: G_START,
        summary: "on a box: print its join request, then keep the admission an Admin approved",
        purpose: "Run it on the box, with the box's own data dir (never the desktop app's). The first run checks the \
                  agent sandbox, prints the box's join request and fingerprint (public) and stops with exit 4: an Admin \
                  approves it with a passkey on the desktop (workspace menu > Admit a box) or the phone. Run it again \
                  with the sealed code they send back; it keeps the admission (as ligus admit redeem does) and checks \
                  that ligus serve answers. No flag can approve a box.",
        synopsis: &["ligus setup box [--label NAME] [--code - | @FILE] [--json] [--dry-run]"],
        flags: &[
            f("--label NAME", "LIGUS_HOST_LABEL, else the host name", "how the workspace names this box"),
            f("--code - | @FILE", "none", "the sealed admission code, from stdin or an owner-only file"),
            JSON,
            DRY_RUN,
        ],
        env: &["LIGUS_DATA_DIR", "LIGUS_HOST_LABEL", "LIGUS_SERVE_LISTEN"],
        examples: &["LIGUS_DATA_DIR=~/.ligus-serve ligus setup box --label home", "ligus setup box --code - < ./admission"],
        exit: &[(3, "the desktop app's data dir, or no agent sandbox"), (4, "waiting for an Admin's passkey approval")],
        see_also: &["ligus admit request", "ligus help box", "desktop: workspace menu > Admit a box (where the Admin approves)"],
        ..D
    },
    Cmd {
        name: "setup phone",
        group: G_START,
        summary: "check this box can pair a phone, then hand over to ligus pair",
        purpose: "Check this box is a member and ligus serve answers, then stop with exit 4: pairing prints a secret \
                  link, so you run `ligus pair` yourself in your own terminal (never in a job or a logged session). \
                  This command never prints the pairing secret. On the desktop: ⌘K > Add a phone.",
        synopsis: &["ligus setup phone [--json] [--dry-run]"],
        flags: &[JSON, DRY_RUN],
        env: &["LIGUS_SERVE_LISTEN"],
        examples: &["ligus setup phone", "ligus setup phone && ligus pair"],
        exit: &[(3, "the box isn't a member, or ligus serve isn't running"), (4, "run ligus pair in your own terminal")],
        see_also: &["ligus pair", "ligus help phone"],
        ..D
    },
    Cmd {
        name: "setup invite",
        group: G_START,
        summary: "check you can invite, list invites and whether they were accepted",
        purpose: "Check you are an Admin and the workspace has a relay, say what an invited person may do (they join \
                  as a Contributor), and list the invites this workspace issued and which were accepted. The CLI never \
                  creates an invite: the code carries the workspace key, so it is made and shown on the desktop \
                  (People > Invite someone). Exit 4 says so.",
        synopsis: &["ligus setup invite [--json]"],
        flags: &[JSON, DRY_RUN],
        examples: &["ligus setup invite", "ligus setup invite --json | jq '.checks[] | select(.id == \"invite.track\")'"],
        exit: &[(3, "no relay yet"), (4, "create the invite on the desktop"), (6, "inviting needs an Admin")],
        see_also: &["ligus help setup", "desktop: People > Invite someone"],
        ..D
    },
    Cmd {
        name: "setup relay",
        group: G_START,
        summary: "check a relay (community or your own, https only) and switch this device to it",
        purpose: "Check the relay (one request: reachable, access token accepted, or nothing stored for this workspace \
                  yet), switch this device's config.toml to it (written to a temp file and renamed; the token goes to \
                  the secret store), and check again. Plain http is refused except for a relay on this machine \
                  (localhost, 127.0.0.0/8, ::1). Every member's devices must use the same relay.",
        synopsis: &["ligus setup relay [--kind community | --kind own --url URL] [--token - | @FILE] [--json] [--dry-run]"],
        flags: &[
            f("--kind community|own", "community", "the community relay (relay.langstroth.ai, best-effort) or your own"),
            f("--url URL", "required for own", "your relay's https URL"),
            f("--token - | @FILE", "none", "the relay access token (hrt1…), from stdin or an owner-only file"),
            JSON,
            DRY_RUN,
        ],
        env: &["LIGUS_RELAY_URL"],
        conn: true,
        examples: &["ligus setup relay", "ligus setup relay --kind own --url https://relay.example.com --token @./token"],
        exit: &[(2, "not https, or a URL with a user name or token"), (3, "the relay wants an access token"), (7, "the relay didn't answer")],
        see_also: &["langstroth-relay check --url URL (on the relay host)", "ligus doctor", "ligus help troubleshooting"],
        ..D
    },
    Cmd {
        name: "setup runtimes",
        group: G_START,
        summary: "detect providers on this box; --apply defines keyless runtimes",
        purpose: "Detect what this box can reach: API keys in the environment, Ollama on 127.0.0.1:11434, and the \
                  claude, codex, agy and copilot CLIs (with their sign-in state). Key-backed providers are printed \
                  as `ligus add-runtime … --secret-ref` commands; --apply defines the keyless ones as workspace \
                  runtimes (share them with `ligus sync`). This was `ligus setup` before the setup flows.",
        synopsis: &["ligus setup runtimes [--apply]"],
        flags: &[f("--apply", "off", "define the detected keyless runtimes in the workspace")],
        env: &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY", "PATH"],
        examples: &["ligus setup runtimes", "ligus setup runtimes --apply && ligus sync"],
        see_also: &["ligus runtimes", "ligus add-runtime", "ligus help agents"],
        ..D
    },
    Cmd {
        name: "app",
        group: G_START,
        summary: "open the desktop app, detached from this terminal",
        purpose: "Open the Ligus desktop app (or bring it forward) and return at once. It never opens the data dir, \
                  the config or the secret store. macOS: LIGUS_APP, then /Applications/Ligus.app, then \
                  ~/Applications/Ligus.app, then (last resort) the copies Spotlight knows by bundle id, never one on \
                  a mounted disk image (/Volumes) or an App Translocation copy unless LIGUS_APP names it; the bundle is \
                  opened by path. When the app's version differs from this CLI's, a note on stderr says which to \
                  update, and the app opens anyway. When a copy is already running from another path, it says so on \
                  stderr and opens nothing. Windows: the installer's registry keys, the default install dirs, then \
                  PATH; Linux: ligus-desktop on PATH, then an AppImage (needs DISPLAY or WAYLAND_DISPLAY). --json \
                  prints ligus.app.v1: path, version, cliVersion, versionMatches, launched, alreadyRunning, \
                  runningFrom, note.",
        synopsis: &["ligus app [--json]"],
        flags: &[JSON],
        data: false,
        env: &["LIGUS_APP", "LIGUS_APPIMAGE", "HOME", "DISPLAY", "WAYLAND_DISPLAY", "PATH"],
        examples: &["ligus app", "LIGUS_APP=\"/Volumes/Ligus 0.2.1/Ligus.app\" ligus app --json"],
        exit: &[(1, "the app isn't installed (the message lists where it looked), LIGUS_APP or LIGUS_APPIMAGE names \
                     no app, or no graphical session on Linux")],
        see_also: &["ligus help quickstart"],
        ..D
    },
    Cmd {
        name: "auth",
        group: G_START,
        summary: "sign in to an organization's relay through langstroth-auth",
        purpose: "Device-code sign-in at langstroth-auth. While a session exists every command uses its short-lived \
                  relay token and refreshes it in the background; LIGUS_RELAY_ACCESS_TOKEN, when set, wins. Signing \
                  in opens the relay only: joining a workspace still takes an invite and an Admin.",
        synopsis: &["ligus auth login <https://auth.example.com/t/<tenant>> [--label NAME]", "ligus auth status", "ligus auth logout"],
        flags: &[f("--label NAME", "$HOSTNAME, else \"ligus CLI\"", "the name this box signs in under (login)")],
        env: &["LIGUS_RELAY_URL", "LIGUS_RELAY_ACCESS_TOKEN", "HOSTNAME"],
        examples: &["ligus auth login https://auth.example.com/t/acme --label build-box", "ligus auth status"],
        see_also: &["ligus auth login", "ligus enroll"],
        ..D
    },
    Cmd {
        name: "auth login",
        group: G_START,
        summary: "device-code sign-in; saves <data dir>/ligus-auth.json",
        purpose: "Print a code to enter in a browser, wait for approval, then save the session (owner-only) in the \
                  data dir.",
        synopsis: &["ligus auth login <url> [--label NAME]"],
        flags: &[f("--label NAME", "$HOSTNAME, else \"ligus CLI\"", "the name this box signs in under")],
        env: &["HOSTNAME", "LIGUS_RELAY_URL"],
        examples: &["ligus auth login https://auth.example.com/t/acme"],
        see_also: &["ligus auth status", "ligus auth logout"],
        ..D
    },
    Cmd {
        name: "auth status",
        group: G_START,
        summary: "the signed-in tenant, subject, workspaces and token lifetime",
        purpose: "Print the tenant, subject, granted workspaces and access-token lifetime (never a token).",
        synopsis: &["ligus auth status"],
        env: &["LIGUS_RELAY_ACCESS_TOKEN"],
        examples: &["ligus auth status"],
        see_also: &["ligus auth login"],
        ..D
    },
    Cmd {
        name: "auth logout",
        group: G_START,
        summary: "revoke the session and delete it",
        purpose: "Revoke the session at langstroth-auth and delete <data dir>/ligus-auth.json.",
        synopsis: &["ligus auth logout"],
        examples: &["ligus auth logout"],
        see_also: &["ligus auth login"],
        ..D
    },
    Cmd {
        name: "config",
        group: G_START,
        summary: "export or import the workspace runtime set as TOML",
        purpose: "Round-trip the workspace runtimes. `export` prints them as TOML (no key is ever exported: a key an \
                  older version stored in the workspace becomes a secretRef named after the runtime); `import` adds \
                  or replaces runtimes by id from a file and refuses one with an inline key. Share imports with \
                  `ligus sync`.",
        synopsis: &["ligus config export", "ligus config import <file.toml>"],
        conn: true,
        examples: &["ligus config export > runtimes.toml", "ligus config import runtimes.toml && ligus sync"],
        see_also: &["ligus config export", "ligus config import", "ligus runtimes"],
        ..D
    },
    Cmd {
        name: "config export",
        group: G_START,
        summary: "print the workspace runtimes as TOML",
        purpose: "Print the workspace runtimes as TOML on stdout; a legacy inline key becomes a secretRef (its value is never read).",
        synopsis: &["ligus config export"],
        conn: true,
        examples: &["ligus config export > runtimes.toml"],
        see_also: &["ligus config import"],
        ..D
    },
    Cmd {
        name: "config import",
        group: G_START,
        summary: "add or replace workspace runtimes from a TOML file",
        purpose: "Read `[[runtime]]` tables from a file and add or replace each by id. `ligus sync` shares them.",
        synopsis: &["ligus config import <file.toml>"],
        conn: true,
        examples: &["ligus config import runtimes.toml && ligus sync"],
        see_also: &["ligus config export"],
        ..D
    },
    // ── Chats ──────────────────────────────────────────────────────────
    Cmd {
        name: "chats",
        group: G_CHAT,
        summary: "list chats in the local store",
        purpose: "List the chats this data dir holds (id, title, message count, channel). Run `ligus sync` first to \
                  pull a workspace.",
        synopsis: &["ligus chats [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus chats", "ligus chats --json"],
        see_also: &["ligus new", "ligus tail", "ligus send"],
        ..D
    },
    Cmd {
        name: "new",
        group: G_CHAT,
        summary: "create a chat and print its id",
        purpose: "Create a chat in this workspace and print its id. A workspace this CLI founds isn't secured until \
                  its founder creates a passkey and a recovery key in the desktop app (a note on stderr says so).",
        synopsis: &["ligus new [title…]"],
        conn: true,
        examples: &["ligus new Release checklist", "CHAT=$(ligus new Triage) && ligus send \"$CHAT\" hello --push"],
        see_also: &["ligus chats", "ligus send"],
        ..D
    },
    Cmd {
        name: "tail",
        group: G_CHAT,
        summary: "print a chat's transcript (--watch to follow)",
        purpose: "Print a chat's messages; --watch polls every 3 s (syncing first when a relay is set) and prints new \
                  ones until stopped.",
        synopsis: &["ligus tail <chat-id> [--watch]"],
        flags: &[f("--watch", "off", "follow the chat")],
        conn: true,
        exit: &[(5, "unknown chat")],
        examples: &["ligus tail 6f1c2a9b-…", "ligus tail 6f1c2a9b-… --watch"],
        see_also: &["ligus send", "ligus chats"],
        ..D
    },
    Cmd {
        name: "send",
        group: G_CHAT,
        summary: "post a signed message (--push to sync it out)",
        purpose: "Post a message, signed by this device, to a chat; the words after the chat id are the message. \
                  --push runs one sync round right after. A word that starts with -- is read as a flag, not text.",
        synopsis: &["ligus send <chat-id> <message…> [--push] [--wait [--timeout DURATION]]"],
        flags: &[
            f("--push", "off", "sync the message out at once"),
            f("--wait", "off", "push, then wait for someone else's answer in the chat and print it"),
            f("--timeout DURATION", "5m with --wait", "how long --wait waits (90s, 5m, 1h; implies --wait); no answer by then exits 7"),
        ],
        conn: true,
        examples: &[
            "ligus send 6f1c2a9b-… \"@coder fix the flaky test\" --push",
            "ligus send 6f1c2a9b-… \"@coder status?\" --wait --timeout 10m",
        ],
        exit: &[(5, "unknown chat"), (7, "--wait: no answer before --timeout, or the relay can't be reached")],
        see_also: &["ligus tail", "ligus sync"],
        ..D
    },
    Cmd {
        name: "proposals",
        group: G_CHAT,
        summary: "list proposals with status and votes",
        purpose: "Every proposal in the workspace's chats, newest first, with its status and the votes cast (who, at \
                  what role).",
        synopsis: &["ligus proposals"],
        conn: true,
        examples: &["ligus proposals"],
        see_also: &["ligus queue"],
        ..D
    },
    Cmd {
        name: "queue",
        group: G_CHAT,
        summary: "unanswered agent mentions and whether each agent's host is online",
        purpose: "Show queued work: every unanswered @mention of an agent, and whether that agent's host is online \
                  (from the relay's presence beats, else the durable record).",
        synopsis: &["ligus queue"],
        conn: true,
        examples: &["ligus queue"],
        see_also: &["ligus set-agent-host", "ligus hosts", "ligus help worker"],
        ..D
    },
    Cmd {
        name: "task-state",
        group: G_CHAT,
        summary: "what the agents on a chat report about their work in progress",
        purpose: "Print the live task state the agents on a chat publish.",
        synopsis: &["ligus task-state <chat-id> [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus task-state 6f1c2a9b-… --json"],
        see_also: &["ligus memory"],
        ..D
    },
    Cmd {
        name: "asks",
        group: G_CHAT,
        summary: "agents' questions: list them, answer one at the terminal",
        purpose: "Agents' decision cards from this person's own CLI device. Every answer needs your yes at the \
                  terminal (/dev/tty): without one it exits 4, so an agent running as you can't settle a card by \
                  accident. A grant (Allow once / Deny) needs your passkey, which the CLI can't ask for: answer it in \
                  the desktop app or on your phone (exit 4). Prompts from `ligus hook permission` are never answered \
                  here (exit 6).",
        synopsis: &[
            "ligus asks [list] [--chat ID] [--mine] [--json]",
            "ligus asks answer <ask-id> (--option N | --yes | --no) [--text TEXT] [--json]",
            "ligus asks decline <ask-id> [--json]",
        ],
        flags: &[
            f("--chat ID", "every chat", "only this chat"),
            f("--mine", "off", "only questions you can answer"),
            f("--option N", "none", "the option to answer with, from 0"),
            f("--yes, --no", "none", "answer a yes/no question"),
            f("--text TEXT", "none", "a free-text reply, where the question allows one"),
            JSON,
        ],
        conn: true,
        examples: &["ligus asks list --mine", "ligus asks answer 0b7e2f8a-… --option 1"],
        exit: &[(4, "no yes at the terminal, or a grant (needs your passkey)"), (5, "no such question"), (6, "not yours to answer, an outside prompt, or refused by the workspace rules")],
        see_also: &["ligus asks list", "ligus asks answer", "ligus hook"],
        ..D
    },
    Cmd {
        name: "asks list",
        group: G_CHAT,
        summary: "open questions, and whether you can answer them",
        purpose: "List the open decision cards: who asks whom, the options, and what an answer does.",
        synopsis: &["ligus asks [list] [--chat ID] [--mine] [--json]"],
        flags: &[f("--chat ID", "every chat", "only this chat"), f("--mine", "off", "only questions you can answer"), JSON],
        conn: true,
        examples: &["ligus asks list --json"],
        see_also: &["ligus asks answer"],
        ..D
    },
    Cmd {
        name: "asks answer",
        group: G_CHAT,
        summary: "answer a question, after your yes at the terminal",
        purpose: "Answer one card with an option (or yes/no, or free text where allowed). Asks for your yes on \
                  /dev/tty first. Grants and outside prompts are refused here.",
        synopsis: &["ligus asks answer <ask-id> (--option N | --yes | --no) [--text TEXT] [--json]"],
        flags: &[f("--option N", "none", "the option, from 0"), f("--yes, --no", "none", "for a yes/no question"), f("--text TEXT", "none", "a free-text reply"), JSON],
        conn: true,
        examples: &["ligus asks answer 0b7e2f8a-… --yes"],
        exit: &[(4, "no yes at the terminal, or a grant"), (5, "no such question"), (6, "not yours to answer")],
        see_also: &["ligus asks decline"],
        ..D
    },
    Cmd {
        name: "asks decline",
        group: G_CHAT,
        summary: "say Not now",
        purpose: "Say Not now: it closes a question put to you alone; on a room question it is only your abstention.",
        synopsis: &["ligus asks decline <ask-id> [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus asks decline 0b7e2f8a-…"],
        exit: &[(4, "no yes at the terminal"), (5, "no such question"), (6, "not yours to answer")],
        see_also: &["ligus asks answer"],
        ..D
    },
    Cmd {
        name: "hook",
        group: G_AGENTS,
        summary: "send Claude Code's permission prompts to your Ligus devices",
        purpose: "Claude Code's PermissionRequest hook, for sessions Ligus didn't start. `ligus hook permission` \
                  reads the prompt on stdin, asks the running Ligus desktop over an owner-only socket in its data \
                  dir (with the install's hook token), and the prompt becomes a grant card in the chat you name, put \
                  to you alone. Allowing it always needs your passkey (Face ID on the phone). It prints Claude Code's \
                  allow or deny, never a standing rule; with no answer, or no Ligus running, it prints nothing and \
                  Claude Code decides (its own dialog, or a deny in -p). The hook never opens a data dir. Point it at \
                  a workspace of your own: every member of a workspace can read its chats.",
        synopsis: &[
            "ligus hook permission --workspace W --chat C [--timeout SECS] [--data-dir DIR]",
            "ligus hook install --settings PATH --workspace W --chat C [--dry-run]",
            "ligus hook token [--data-dir DIR] [--json]",
        ],
        flags: &[
            f("--workspace W", "required", "the workspace id (one of your own)"),
            f("--chat C", "required", "the chat id the cards go to"),
            f("--timeout SECS", "90", "how long the hook waits (10 to 590)"),
            f("--data-dir DIR", "the desktop's", "the running Ligus process to ask"),
            f("--settings PATH", "required", "the Claude Code settings file to write the hook into"),
            f("--dry-run", "off", "print the change, write nothing"),
            JSON,
        ],
        data: false,
        examples: &["ligus hook install --settings .claude/settings.json --workspace 1b2c… --chat 6f1c…", "ligus hook token"],
        see_also: &["ligus hook permission", "ligus hook install", "ligus asks"],
        ..D
    },
    Cmd {
        name: "hook permission",
        group: G_AGENTS,
        summary: "the PermissionRequest hook itself (Claude Code runs it)",
        purpose: "Read Claude Code's PermissionRequest input on stdin and print its decision from Ligus: \
                  {\"hookSpecificOutput\":{\"hookEventName\":\"PermissionRequest\",\"decision\":{\"behavior\":\"allow\"}}}, \
                  or a deny with \"Denied from Ligus by <name>\", or nothing (no answer in time, Not now, no Ligus \
                  running, more than 10 prompts a minute or 3 waiting). Always exits 0. The card shows the call's own \
                  content: the command, or the file and its new content; one over 8 KiB offers only Deny.",
        synopsis: &["ligus hook permission --workspace W --chat C [--timeout SECS] [--data-dir DIR]"],
        flags: &[
            f("--workspace W", "required", "the workspace id"),
            f("--chat C", "required", "the chat id"),
            f("--timeout SECS", "90", "how long to wait (10 to 590; keep it under the hook's timeout)"),
            f("--data-dir DIR", "the desktop's", "the running Ligus process to ask"),
        ],
        env: &["LIGUS_DATA_DIR"],
        data: false,
        examples: &["echo '{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"ls\"}}' | ligus hook permission --workspace 1b2c… --chat 6f1c…"],
        see_also: &["ligus hook install"],
        ..D
    },
    Cmd {
        name: "hook install",
        group: G_AGENTS,
        summary: "write the hook into a Claude Code settings file",
        purpose: "Add (or update) the PermissionRequest hook in a Claude Code settings file you name, keeping \
                  everything else, and print the change. Running it again changes nothing. Your own \
                  ~/.claude/settings.json needs your yes at the terminal (exit 4 without it).",
        synopsis: &["ligus hook install --settings PATH --workspace W --chat C [--dry-run]"],
        flags: &[
            f("--settings PATH", "required", "the settings file (.claude/settings.json for one project)"),
            f("--workspace W", "required", "the workspace id"),
            f("--chat C", "required", "the chat id"),
            f("--dry-run", "off", "print the change, write nothing"),
        ],
        data: false,
        examples: &["ligus hook install --settings .claude/settings.json --workspace 1b2c… --chat 6f1c… --dry-run"],
        exit: &[(4, "your user-level settings need your yes at the terminal")],
        see_also: &["ligus hook permission"],
        ..D
    },
    Cmd {
        name: "hook token",
        group: G_AGENTS,
        summary: "where the install's hook token is",
        purpose: "Print the path of each install's hook token (0600) and whether its socket listens. Never the \
                  token itself, and it creates nothing: the desktop makes its token when it starts listening.",
        synopsis: &["ligus hook token [--data-dir DIR] [--json]"],
        flags: &[f("--data-dir DIR", "the desktop's", "the install"), JSON],
        data: false,
        examples: &["ligus hook token --json"],
        exit: &[(3, "no Ligus data dir here")],
        see_also: &["ligus hook"],
        ..D
    },
    // ── Runtimes, agents, hosts ────────────────────────────────────────
    Cmd {
        name: "runtimes",
        group: G_AGENTS,
        summary: "list workspace-owned runtimes",
        purpose: "List the workspace runtimes synced onto this box: id, name, provider/model and credential (keyless, \
                  secretRef with whether LIGUS_WS_SECRET_<ref> is set here, or \"stored key (unused)\" with a warning \
                  for a key an older version stored in the workspace: rotate it and use --secret-ref). Never prints a \
                  secret.",
        synopsis: &["ligus runtimes [--json]"],
        flags: &[JSON],
        env: &["LIGUS_WS_SECRET_<ref>"],
        conn: true,
        examples: &["ligus runtimes", "ligus runtimes --json"],
        see_also: &["ligus add-runtime", "ligus setup runtimes", "ligus help agents"],
        ..D
    },
    Cmd {
        name: "add-runtime",
        group: G_AGENTS,
        summary: "define (or replace by id) a workspace runtime",
        purpose: "Define a workspace-owned runtime. <provider> is anthropic, openai, openrouter, ollama, custom (any \
                  OpenAI-compatible server), claude-code, codex, antigravity (agy) or copilot. Credential: \
                  --secret-ref NAME: each host that runs it sets LIGUS_WS_SECRET_<NAME> (or a file in \
                  LIGUS_WS_SECRET_DIR); without it the runtime is keyless. Keys are never stored in the workspace: \
                  --secret is a usage error. Share with `ligus sync`.",
        synopsis: &["ligus add-runtime <id> <name> <provider> <model> [--endpoint URL] [--secret-ref NAME] [--dry-run]"],
        flags: &[
            f("--endpoint URL", "the provider's default", "model endpoint (custom, ollama, a gateway)"),
            f("--secret-ref NAME", "none", "hosts read the key from LIGUS_WS_SECRET_<NAME>; keys are never stored in the workspace"),
            DRY_RUN,
        ],
        conn: true,
        examples: &[
            "ligus add-runtime claude \"Claude Code\" claude-code \"\"",
            "ligus add-runtime anthropic Anthropic anthropic claude-sonnet-4-6 --secret-ref anthropic",
            "ligus add-runtime local Gemma custom gemma3:12b --endpoint http://127.0.0.1:8080/v1/chat/completions",
        ],
        see_also: &["ligus runtimes", "ligus add-agent", "ligus help agents"],
        ..D
    },
    Cmd {
        name: "rm-runtime",
        group: G_AGENTS,
        summary: "remove a workspace runtime",
        purpose: "Remove a workspace runtime by id. Share with `ligus sync`.",
        synopsis: &["ligus rm-runtime <id> [--dry-run]"],
        flags: &[DRY_RUN],
        conn: true,
        exit: &[(5, "--dry-run: no runtime with that id")],
        examples: &["ligus rm-runtime local && ligus sync"],
        see_also: &["ligus runtimes"],
        ..D
    },
    Cmd {
        name: "agents",
        group: G_AGENTS,
        summary: "list the workspace's agents",
        purpose: "List the workspace agents: name, runtime, host (an agent with no host is answered by its owner's \
                  device) and whether it may run local servers.",
        synopsis: &["ligus agents [--json]", "ligus agents set <agent> --local-servers on|off [--json]"],
        flags: &[JSON],
        conn: true,
        exit: &[(2, "unknown subcommand")],
        examples: &["ligus agents", "ligus agents --json"],
        see_also: &["ligus agents set", "ligus add-agent", "ligus set-agent-host", "ligus help agents"],
        ..D
    },
    Cmd {
        name: "agents set",
        group: G_AGENTS,
        summary: "let an agent run local servers (or stop it)",
        purpose: "Turn an agent's local-servers switch on or off: a signed workspace sandbox grant, off by default, \
                  the workspace Owner's to change (with a passkey in a shared workspace, so on a CLI device it works \
                  in your local workspace; otherwise use Settings, Sandbox on the desktop). On, the agent may run a \
                  local server such as the Google Antigravity CLI's (agy) helper server. Linux with bubblewrap: every \
                  turn has its own loopback in a private network namespace, where that works. Linux Landlock tier (no \
                  namespaces): it stays off. macOS: not available yet (the Mac's sandbox can't keep a listener \
                  loopback-only), so the switch is stored but has no effect there. Files, unix sockets and egress \
                  never change. Applies where the workspace grants apply (by default \
                  the Owner's own devices). Share with `ligus sync`.",
        synopsis: &["ligus agents set <agent> --local-servers on|off [--json]"],
        flags: &[f("--local-servers on|off", "required", "the agent's local-servers switch"), JSON],
        conn: true,
        exit: &[(2, "no agent named, or --local-servers without on or off"), (5, "no such agent"), (6, "you aren't the workspace Owner, or a shared workspace needs your passkey (use the desktop)")],
        examples: &["ligus agents set gem --local-servers on && ligus sync", "ligus agents set @gem --local-servers off --json"],
        see_also: &["ligus agents", "ligus doctor --sandbox", "ligus setup agent", "desktop: Settings > Sandbox"],
        ..D
    },
    Cmd {
        name: "add-agent",
        group: G_AGENTS,
        summary: "define a workspace agent on a runtime",
        purpose: "Define a workspace agent bound to a workspace runtime, optionally on a host and with a role (a short \
                  description every member sees). Share with `ligus sync`.",
        synopsis: &["ligus add-agent <name> <runtime-id> [--host HOST-ID] [--role TEXT] [--dry-run]"],
        flags: &[
            f("--host HOST-ID", "none (its owner's device)", "the host that answers it (`ligus hosts` lists ids)"),
            f("--role TEXT", "empty", "what the agent does, shown on the roster"),
            DRY_RUN,
        ],
        conn: true,
        exit: &[(5, "--dry-run: no runtime with that id")],
        examples: &["ligus add-agent coder claude --host \"$(ligus whoami --json | jq -r .device)\" --role \"writes code\" && ligus sync"],
        see_also: &["ligus agents", "ligus set-agent-host", "ligus help agents"],
        ..D
    },
    Cmd {
        name: "hosts",
        group: G_AGENTS,
        summary: "list workspace hosts (devices and workers)",
        purpose: "List the workspace's hosts: id, kind, label, and which one is this box.",
        synopsis: &["ligus hosts [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus hosts", "ligus hosts --json"],
        see_also: &["ligus register-worker", "ligus set-agent-host"],
        ..D
    },
    Cmd {
        name: "register-worker",
        group: G_AGENTS,
        summary: "register this box as a worker host",
        purpose: "Register this box as a worker host and print its host id (sync to share). Needs a local copy of the \
                  workspace (`ligus sync` first). `ligus worker` registers itself, so this is only for binding \
                  agents before the first start.",
        synopsis: &["ligus register-worker [--label NAME]"],
        flags: &[f("--label NAME", "$HOSTNAME, else \"worker\"", "the label other members see")],
        env: &["HOSTNAME", "LIGUS_MODEL_GATEWAYS_FILE"],
        conn: true,
        examples: &["ligus register-worker --label build-box && ligus sync"],
        see_also: &["ligus worker", "ligus set-agent-host"],
        ..D
    },
    Cmd {
        name: "set-agent-host",
        group: G_AGENTS,
        summary: "bind an agent to a host",
        purpose: "Bind an agent (by name) to a host id, so that host answers its mentions. Sync to apply.",
        synopsis: &["ligus set-agent-host <agent-name> <host-id> [--dry-run]"],
        flags: &[DRY_RUN],
        conn: true,
        exit: &[(5, "no agent with that name")],
        examples: &["ligus set-agent-host coder 5b1c0f7e-… && ligus sync"],
        see_also: &["ligus hosts", "ligus agents", "ligus queue"],
        ..D
    },
    Cmd {
        name: "agent",
        group: G_AGENTS,
        summary: "development responder on a personal credential (not for production)",
        purpose: "Answer new un-mentioned or @primary messages as an agent, on a workspace runtime (--runtime) or a \
                  personal key from the environment. Refuses to run unless LIGUS_ALLOW_PERSONAL_AGENT is set; in a \
                  shared room it answers only its operator unless LIGUS_AGENT_OPEN_TO_MEMBERS=1. Use `ligus worker` \
                  in production.",
        synopsis: &["ligus agent [name] [--runtime RUNTIME-ID]"],
        flags: &[f("--runtime RUNTIME-ID", "LIGUS_RUNTIME, else a personal key (LIGUS_PROVIDER/LIGUS_MODEL)", "workspace runtime to answer with")],
        env: &[
            "LIGUS_ALLOW_PERSONAL_AGENT",
            "LIGUS_AGENT_OPEN_TO_MEMBERS",
            "LIGUS_RUNTIME",
            "LIGUS_PROVIDER",
            "LIGUS_MODEL",
            "LIGUS_MODEL_BASE_URL",
            "LIGUS_PROVIDER_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "HOSTNAME",
        ],
        conn: true,
        examples: &["LIGUS_ALLOW_PERSONAL_AGENT=1 ligus agent helper --runtime claude"],
        see_also: &["ligus worker", "ligus help agents"],
        ..D
    },
    Cmd {
        name: "trust",
        group: G_AGENTS,
        summary: "repo-supplied runtimes and MCP servers this device runs",
        purpose: "List the runtimes and MCP servers a repository ships and whether this device trusts each (an \
                  untrusted one is INERT), or trust one by digest prefix, or all of them.",
        synopsis: &["ligus trust [list] [--repo PATH]", "ligus trust enable <digest> [--repo PATH]", "ligus trust enable-all [--repo PATH]"],
        flags: &[f("--repo PATH", "the current directory", "the repository checkout")],
        examples: &["ligus trust list --repo ~/src/app", "ligus trust enable 3fa29c1e --repo ~/src/app"],
        see_also: &["ligus trust list", "ligus trust enable", "ligus trust enable-all", "ligus help security"],
        ..D
    },
    Cmd {
        name: "trust list",
        group: G_AGENTS,
        summary: "a repository's runtimes and MCP servers and their trust",
        purpose: "List digest, kind, id, trusted or INERT, source and command line for each item the repository ships.",
        synopsis: &["ligus trust list [--repo PATH]"],
        flags: &[f("--repo PATH", "the current directory", "the repository checkout")],
        examples: &["ligus trust list --repo ~/src/app"],
        see_also: &["ligus trust enable"],
        ..D
    },
    Cmd {
        name: "trust enable",
        group: G_AGENTS,
        summary: "trust one item by digest prefix",
        purpose: "Trust one repo-supplied runtime or MCP server on this device (digest prefix from `ligus trust list`).",
        synopsis: &["ligus trust enable <digest> [--repo PATH]"],
        flags: &[f("--repo PATH", "the current directory", "the repository checkout")],
        examples: &["ligus trust enable 3fa29c1e --repo ~/src/app"],
        see_also: &["ligus trust list"],
        ..D
    },
    Cmd {
        name: "trust enable-all",
        group: G_AGENTS,
        summary: "trust everything a repository ships",
        purpose: "Trust every runtime and MCP server the repository ships, on this device.",
        synopsis: &["ligus trust enable-all [--repo PATH]"],
        flags: &[f("--repo PATH", "the current directory", "the repository checkout")],
        examples: &["ligus trust enable-all --repo ~/src/app"],
        see_also: &["ligus trust list"],
        ..D
    },
    // ── Boxes ──────────────────────────────────────────────────────────
    Cmd {
        name: "serve",
        group: G_BOX,
        summary: "the home host for the phone app: sync + worker + HTTP/SSE API",
        purpose: "Run the always-on box: sync with the relay, answer mentions for the agents bound here, and serve the \
                  phone app's /v1 API. The box joins as its own service member through an admission (`ligus admit`). \
                  The listener accepts loopback, private LAN, link-local, CGNAT (Tailscale) or IPv6 ULA addresses \
                  only. Runs until SIGTERM or Ctrl-C.",
        synopsis: &[
            "ligus serve [--listen ADDR] [--token-file PATH] [--label NAME] [--name NAME] [--public-url URL]",
            "            [--ntfy-url URL --ntfy-topic T] [--pair] [--remote-relay URL|off] [--reg-token - | @FILE] [--insecure-relay]",
        ],
        flags: &[
            f("--listen ADDR", "LIGUS_SERVE_LISTEN, else 127.0.0.1:8787", "API address (IP:PORT)"),
            f("--token-file PATH", "LIGUS_SERVE_TOKEN_FILE, else <data dir>/serve.token", "the direct-path bearer token file"),
            f("--label NAME", "LIGUS_SERVE_LABEL, else the host name, else \"home host\"", "host label other members see"),
            f("--name NAME", "LIGUS_SERVE_NAME, else the identity's name", "display name the box joins under"),
            f("--public-url URL", "LIGUS_SERVE_PUBLIC_URL, else http://<listen>", "URL put in the pairing link"),
            f("--ntfy-url URL", "LIGUS_NTFY_URL", "self-hosted ntfy server (https://; with --ntfy-topic)"),
            f("--ntfy-topic T", "LIGUS_NTFY_TOPIC", "ntfy topic"),
            f("--pair", "off", "print the pairing QR code at start"),
            f("--remote-relay URL|off", "LIGUS_REMOTE_RELAY, else tunnel/relay.json", "serve the phone through this relay tunnel; off disables it for this run"),
            f("--reg-token - | @FILE", "LIGUS_REMOTE_REG_TOKEN, else relay.json", "registration token for a gated relay"),
            f("--insecure-relay", "LIGUS_REMOTE_INSECURE_RELAY, else off", "allow an http:// relay (tests, loopback)"),
        ],
        env: &[
            "LIGUS_SERVE_LISTEN",
            "LIGUS_SERVE_ALLOW_ANY_BIND",
            "LIGUS_SERVE_TOKEN_FILE",
            "LIGUS_SERVE_LABEL",
            "LIGUS_SERVE_NAME",
            "LIGUS_SERVE_PUBLIC_URL",
            "LIGUS_SERVE_DEFAULT_AGENT",
            "LIGUS_SERVE_PALETTE",
            "LIGUS_SERVE_APPEARANCE",
            "LIGUS_DESKTOP_DATA_DIR",
            "LIGUS_SERVE_OWNER",
            "LIGUS_ADMISSION_FILE",
            "LIGUS_REQUIRE_SANDBOX",
            "LIGUS_REMOTE_RELAY",
            "LIGUS_REMOTE_REG_TOKEN",
            "LIGUS_REMOTE_INSECURE_RELAY",
            "LIGUS_NTFY_URL",
            "LIGUS_NTFY_TOPIC",
            "LIGUS_NTFY_TOKEN",
            "LIGUS_NTFY_ALLOW_HTTP",
            "LIGUS_NTFY_TITLES",
            "LIGUS_DASHBOARD_FILE",
            "LIGUS_PROMETHEUS_URL",
            "LIGUS_LAB_TEMPS",
            "LIGUS_REPOS_DIR",
            "LIGUS_REPOS_MAX_CLONE_MB",
            "LIGUS_REPOS_CLONE_TIMEOUT_SECS",
            "LIGUS_MCP_CONFIG",
            "LIGUS_WORKER_PERMISSION_MODE",
            "LIGUS_WORKER_TOOLS",
            "LIGUS_RUNTIMES_FILE",
            "LIGUS_AUTH_ADMIN_URL",
            "LIGUS_AUTH_ADMIN_TENANT",
            "LIGUS_AUTH_ADMIN_TOKEN_FILE",
            "LIGUS_WS_SECRET_<ref>",
        ],
        conn: true,
        examples: &[
            "ligus serve --listen 127.0.0.1:8787 --label home-box",
            "ligus serve --listen 100.101.102.103:8787 --pair",
            "ligus serve --remote-relay https://relay.example.com --reg-token @/etc/ligus/reg-token",
        ],
        exit: &[(3, "LIGUS_REQUIRE_SANDBOX is on and this host has no agent sandbox")],
        see_also: &["ligus help box", "ligus admit", "ligus pair", "ligus remote", "ligus help phone"],
        ..D
    },
    Cmd {
        name: "worker",
        group: G_BOX,
        summary: "the worker daemon: answer queued mentions for the agents bound here",
        purpose: "Register this box as a worker host, sync, and every 3 s answer the unanswered @mentions of the agents \
                  bound to it (backlog included), on workspace runtimes only. Joins as its own service member through \
                  an admission. Publishes a liveness beat, backs off on relay errors (6 s doubling to 120 s), \
                  finishes replies interrupted by the last stop, and exits cleanly on SIGTERM or Ctrl-C (a turn in \
                  flight gets 12 s).",
        synopsis: &["ligus worker [--label NAME] [--health-listen ADDR]"],
        flags: &[
            f("--label NAME", "$HOSTNAME, else \"worker\"", "host label other members see"),
            f("--health-listen ADDR", "LIGUS_WORKER_HEALTH_LISTEN, else off", "serve GET /v1/health and /v1/ready on ADDR"),
        ],
        env: &[
            "LIGUS_WORKER_HEALTH_LISTEN",
            "LIGUS_ADMISSION_FILE",
            "LIGUS_REQUIRE_SANDBOX",
            "LIGUS_WORKER_PERMISSION_MODE",
            "LIGUS_WORKER_TOOLS",
            "LIGUS_RUNTIMES_FILE",
            "LIGUS_TURN_TIMEOUT_SECS",
            "LIGUS_BUILTIN_TOOLS",
            "LIGUS_BUILTIN_RUN_COMMAND",
            "LIGUS_ALLOW_REMOTE_FILE_TOOLS",
            "LIGUS_MCP_AFTER_READS",
            "LIGUS_MCP_CONFIG",
            "LIGUS_MCP_MIN_ROLE",
            "LIGUS_REPOS_DIR",
            "LIGUS_WS_SECRET_<ref>",
            "LIGUS_WS_SECRET_DIR",
            "LIGUS_MODEL_GATEWAYS_FILE",
            "HOSTNAME",
        ],
        conn: true,
        examples: &["ligus worker --label build-box", "ligus worker --label build-box --health-listen 127.0.0.1:8788"],
        exit: &[(3, "LIGUS_REQUIRE_SANDBOX is on and this host has no agent sandbox")],
        see_also: &["ligus help worker", "ligus admit", "ligus queue", "ligus doctor"],
        ..D
    },
    Cmd {
        name: "approvals",
        group: G_BOX,
        summary: "approval requests in the workspace's mailbox (code match)",
        purpose: "Boxes and computers that asked through the approval address (`ligus admit address`). List them, or \
                  approve one headless with the code its screen shows and your recovery key. Prefer the desktop or the \
                  phone: they show the code next to Codes match.",
        synopsis: &["ligus approvals list [--json]", "ligus approvals approve <rid> [--code -] --recovery-words-file PATH [--json]"],
        conn: true,
        examples: &["ligus approvals list --json"],
        see_also: &["ligus approvals list", "ligus approvals approve", "ligus admit address"],
        ..D
    },
    Cmd {
        name: "approvals list",
        group: G_BOX,
        summary: "the live approval requests: kind, label, fingerprint, who is checking each",
        purpose: "Read the approval mailboxes of the current key epoch and the previous two, verify each request, and \
                  list the live ones: rid, kind (box, computer, phone), label, fingerprint, when it was asked, the relay, \
                  and who claimed it (unclaimed, this device, another approver, or contested: opened by a device that \
                  can't approve it). No code: a code exists only once an approver opens a request.",
        synopsis: &["ligus approvals list [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus approvals list --json"],
        exit: &[(3, "no relay, no key, or the relay has no approval mailbox"), (7, "the relay is unreachable")],
        see_also: &["ligus approvals approve"],
        ..D
    },
    Cmd {
        name: "approvals approve",
        group: G_BOX,
        summary: "headless approver: open a request, compare its code, approve with your recovery key",
        purpose: "A person's own step, like `ligus admit issue`. Opens the request (the requesting device then shows \
                  a code), asks you to type the code it shows (or, with --code -, reads it from stdin), and approves only when it equals the \
                  code this device computes; the CLI never offers the code. A box: an Admin issues the admission (recovery key), publishes it and sends it sealed \
                  to the box. A computer of yours: the grant is built, the computer acknowledges it, then the certificate \
                  is published with your recovery key and the grant sent. A wrong code ends the request and nothing is \
                  granted; an expired code or an ended request writes nothing. A request another approver is checking \
                  is refused (exit 6); a contested one (opened by a device that can't approve it) is never approved: \
                  --end ends it.",
        synopsis: &["ligus approvals approve <rid> [--code -] --recovery-words-file PATH [--json]", "ligus approvals approve <rid> --end [--json]"],
        flags: &[
            f("--code -", "asked at a terminal", "read the code from stdin once the request is opened (the code exists only then); case, spaces, I/L/O forgiven"),
            f("--end", "off", "end a contested request (opened by a device that can't approve it) instead of approving it"),
            f("--recovery-words-file PATH", "required", "your recovery key's 24 words (an owner-only file): your presence"),
            JSON,
        ],
        conn: true,
        examples: &["ligus approvals approve 3f2a…c9 --recovery-words-file ~/.ligus-words"],
        exit: &[
            (2, "--code with a value: the code exists only once the request is opened (type it, or --code -)"),
            (4, "no terminal and no --code -, or no --recovery-words-file: a person's step"),
            (5, "no live request with that rid"),
            (4, "the request is contested: end it with --end"),
            (6, "refused: another approver is checking it, not an Admin (box), a phone (use the desktop), lockdown"),
            (7, "the relay is unreachable, or the requesting device didn't answer"),
        ],
        see_also: &["ligus approvals list", "ligus admit issue", "ligus devices approve"],
        ..D
    },
    Cmd {
        name: "admit",
        group: G_BOX,
        summary: "box admission: request, redeem, issue, close",
        purpose: "A serve or worker box joins a workspace as its own service member, never as a person's device. The \
                  box prints a join request; an Admin approves it (desktop: workspace menu > Admit a box, or the \
                  phone) and hands back a code sealed to the box; the box redeems it and joins on its next start.",
        synopsis: &[
            "ligus admit address [--reset] [--json]",
            "ligus admit request [--label NAME]",
            "ligus admit request --to ligusaddr1:… [--label NAME] [--wait 15m] [--json]",
            "ligus admit redeem <ligusadmit1:code> | - | --file PATH",
            "ligus admit issue --recovery-words-file PATH [--ttl DUR] [--request ligusjoin1:…] [--out PATH]",
            "ligus admit close",
        ],
        flags: &[
            f("--label NAME", "LIGUS_SERVE_LABEL, else the host name, else \"box\"", "request: the label in the join request"),
            f("--file PATH", "none", "redeem: read the code from a file"),
        ],
        env: &["LIGUS_SERVE_LABEL", "LIGUS_ADMISSION_FILE"],
        conn: true,
        examples: &["ligus admit request --label home-box", "ligus admit redeem ligusadmit1:…", "ligus admit redeem --file ./admission"],
        see_also: &["ligus admit address", "ligus admit request", "ligus admit redeem", "ligus admit issue", "ligus admit close", "ligus approvals", "ligus help box"],
        ..D
    },
    Cmd {
        name: "admit address",
        group: G_BOX,
        summary: "print the workspace's approval address (ligusaddr1:) for boxes and computers",
        purpose: "On any member: register the workspace's approval mailbox on its relay (idempotent) and print the \
                  approval address (`ligusaddr1:…`). It is not a secret and grants nothing: a box or a computer sends its \
                  request there (`--to`) and it reaches Admins' Inboxes and paired phones. --reset (Admins) cuts off a \
                  leaked address: the old one stops taking requests. A key rotation gives a new address; members keep \
                  reading the previous two.",
        synopsis: &["ligus admit address [--reset] [--json]"],
        flags: &[f("--reset", "off", "Admins: register a new put capability, so the old address stops working"), JSON],
        conn: true,
        examples: &["ligus admit address", "ligus admit address --reset --json"],
        exit: &[(3, "no relay, no key yet, or the relay has no approval mailbox"), (6, "--reset by someone who isn't an Admin"), (7, "the relay is unreachable")],
        see_also: &["ligus admit request", "ligus devices request", "ligus approvals"],
        ..D
    },
    Cmd {
        name: "admit request",
        group: G_BOX,
        summary: "print this box's join request for an Admin, or send it to the approval address",
        purpose: "Make this data dir a box (a service member; one way) and print its join request (`ligusjoin1:…`) and \
                  fingerprint for an Admin to approve. Changes no workspace state. With --to the request also goes to the \
                  workspace's approval address: it reaches Admins' Inboxes and phones, both screens show the same \
                  8-symbol code (printed here as `Code: K7Q4 M9X2`), and at a terminal this asks whether the approver's \
                  screen shows it. Without a terminal the box accepts only after checking the approval on the real \
                  workspace log (signed by an Admin's device, with a passkey), then keeps the admission like `ligus admit \
                  redeem`. The paste text is printed straight away for an older desktop.",
        synopsis: &["ligus admit request [--label NAME]", "ligus admit request --to ligusaddr1:… [--label NAME] [--wait 15m] [--json]"],
        flags: &[
            f("--label NAME", "LIGUS_SERVE_LABEL, else the host name, else \"box\"", "the label in the request"),
            f("--to ligusaddr1:…", "none (print the request to paste)", "send the request to the approval address (an Admin prints it: ligus admit address)"),
            f("--wait DUR", "15m", "with --to: how long to wait for an approver (90s, 5m, 1h)"),
            f("--insecure-relay", "off", "with --to: allow an http:// relay that isn't loopback (tests)"),
            JSON,
        ],
        env: &["LIGUS_SERVE_LABEL"],
        conn: true,
        examples: &["ligus admit request --label home-box", "ligus admit request --to ligusaddr1:… --label build-1 --json && ligus serve"],
        exit: &[
            (3, "with --to: the relay has no approval mailbox (paste the request instead: ligus admit request)"),
            (4, "with --to: nobody approved within --wait, or the address is stale (ask an Admin for the current one)"),
            (6, "with --to: the relay refused the access token"),
            (7, "with --to: the relay is unreachable, rate-limited or its mailbox is full"),
        ],
        see_also: &["ligus admit redeem", "ligus help box", "desktop: workspace menu > Admit a box (where the Admin approves)"],
        ..D
    },
    Cmd {
        name: "admit redeem",
        group: G_BOX,
        summary: "keep a box admission; the next serve/worker start joins with it",
        purpose: "Keep an admission code for this box: its relay, room, workspace key and the invite it redeems when \
                  it adds itself. A sealed `ligusadmit1:` code may be on the command line; a bearer `hivews1:` code \
                  carries the workspace key and is read only from stdin or a file. LIGUS_ADMISSION_FILE does the same \
                  at start-up.",
        synopsis: &["ligus admit redeem <ligusadmit1:code>", "ligus admit redeem -", "ligus admit redeem --file PATH"],
        flags: &[f("-", "", "read the code from stdin"), f("--file PATH", "none", "read the code from a file")],
        conn: true,
        examples: &["ligus admit redeem ligusadmit1:…", "ligus admit redeem --file ./admission && ligus serve"],
        see_also: &["ligus admit request", "ligus serve", "ligus worker"],
        ..D
    },
    Cmd {
        name: "admit issue",
        group: G_BOX,
        summary: "headless Admin: issue a box admission confirmed with a recovery key",
        purpose: "Issue a box admission from your own (non-box) CLI identity, confirmed with your recovery key. With \
                  --request the code is sealed to that box. Prefer the desktop or the phone. Sync to publish it.",
        synopsis: &["ligus admit issue --recovery-words-file PATH [--ttl DUR] [--request ligusjoin1:…] [--out PATH]"],
        flags: &[
            f("--recovery-words-file PATH", "required", "your recovery key's 24 words (owner-only file)"),
            f("--ttl DUR", "1h with --request, else 24h", "lifetime: 90m, 1h, 2d or seconds (1 minute to just under 7 days)"),
            f("--request ligusjoin1:…", "none (a bearer code)", "the box's join request to seal the code to"),
            f("--out PATH", "stdout", "write the code owner-only (0600) into an existing directory"),
        ],
        conn: true,
        examples: &["ligus admit issue --recovery-words-file ~/.ligus-words --request ligusjoin1:… --out ./admission && ligus sync"],
        see_also: &["ligus admit redeem", "ligus recovery-key create"],
        ..D
    },
    Cmd {
        name: "admit close",
        group: G_BOX,
        summary: "close settled box admissions (Admin CLI identity)",
        purpose: "Close this workspace's settled box admissions (redeemed, or lapsed unredeemed) so no later or \
                  backdated redemption admits anyone. The desktop does it after every sync. Run `ligus sync` before \
                  and after.",
        synopsis: &["ligus admit close"],
        conn: true,
        examples: &["ligus sync && ligus admit close && ligus sync"],
        see_also: &["ligus admit issue"],
        ..D
    },
    Cmd {
        name: "recovery-key",
        group: G_BOX,
        summary: "register a recovery key for this CLI identity (never a box)",
        purpose: "Register a recovery key for this CLI identity (a CLI founder's first credential, or with presence it \
                  already holds) and write its 24 words to PATH (owner-only; the directory must exist).",
        synopsis: &["ligus recovery-key create --out PATH"],
        flags: &[f("--out PATH", "required", "where to write the words; must not exist")],
        conn: true,
        examples: &["ligus recovery-key create --out ~/.ligus-words"],
        see_also: &["ligus recovery-key create", "ligus admit issue", "ligus phones certify"],
        ..D
    },
    Cmd {
        name: "recovery-key create",
        group: G_BOX,
        summary: "create and register a recovery key, words to a file",
        purpose: "Write the 24 words to PATH (0600) and register the key. Refused on a box and when PATH exists.",
        synopsis: &["ligus recovery-key create --out PATH"],
        flags: &[f("--out PATH", "required", "where to write the words; must not exist")],
        conn: true,
        examples: &["ligus recovery-key create --out ~/.ligus-words && ligus sync"],
        see_also: &["ligus admit issue"],
        ..D
    },
    // ── Phone and remote ───────────────────────────────────────────────
    Cmd {
        name: "pair",
        group: G_PHONE,
        summary: "print the phone pairing QR code and link",
        purpose: "Print the pairing QR code and link for a configured (usually running) `ligus serve`, without starting \
                  one. Reads the server's token and never creates one. A loopback URL gets a relay-only link when \
                  remote access is on. Pairing links are one-time, last 10 minutes and allow five attempts.",
        synopsis: &["ligus pair [--listen ADDR] [--token-file PATH] [--public-url URL] [--relay-only] [--remote-relay URL|off] [--reg-token - | @FILE] [--insecure-relay]"],
        flags: &[
            f("--listen ADDR", "LIGUS_SERVE_LISTEN, else 127.0.0.1:8787", "the serve address (to build the direct link)"),
            f("--token-file PATH", "LIGUS_SERVE_TOKEN_FILE, else <data dir>/serve.token", "the serve token file"),
            f("--public-url URL", "LIGUS_SERVE_PUBLIC_URL, else http://<listen>", "URL in the link"),
            f("--relay-only", "off", "a link that uses the relay tunnel only"),
            f("--remote-relay URL|off", "LIGUS_REMOTE_RELAY, else tunnel/relay.json", "the relay tunnel to include"),
            f("--reg-token - | @FILE", "LIGUS_REMOTE_REG_TOKEN", "registration token for a gated relay"),
            f("--insecure-relay", "off", "allow an http:// relay"),
        ],
        env: &["LIGUS_SERVE_LISTEN", "LIGUS_SERVE_TOKEN_FILE", "LIGUS_SERVE_PUBLIC_URL", "LIGUS_SERVE_ALLOW_ANY_BIND", "LIGUS_REMOTE_RELAY", "LIGUS_REMOTE_REG_TOKEN", "LIGUS_REMOTE_INSECURE_RELAY"],
        conn: true,
        examples: &["ligus pair", "ligus pair --relay-only", "set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a; ligus pair"],
        see_also: &["ligus help phone", "ligus serve", "ligus remote enable"],
        ..D
    },
    Cmd {
        name: "remote",
        group: G_PHONE,
        summary: "phone remote access through a relay tunnel (off by default)",
        purpose: "Let the phone reach `ligus serve` through a relay instead of only the LAN or tailnet. These commands \
                  write files under <data dir>/tunnel that a running serve reads; restart serve after enable, \
                  disable or reset.",
        synopsis: &[
            "ligus remote enable --relay URL [--reg-token - | @FILE] [--insecure-relay]",
            "ligus remote [status] [--json]",
            "ligus remote push-test [--phone PID|NAME]",
            "ligus remote disable",
            "ligus remote reset",
        ],
        flags: &[JSON],
        examples: &["ligus remote enable --relay https://relay.example.com", "ligus remote status --json"],
        see_also: &["ligus remote enable", "ligus remote status", "ligus remote push-test", "ligus remote disable", "ligus remote reset", "ligus help remote"],
        ..D
    },
    Cmd {
        name: "remote enable",
        group: G_PHONE,
        summary: "turn on the relay tunnel (creates host keys, writes tunnel/relay.json)",
        purpose: "Turn on phone access through a relay tunnel: create this box's host keys and write \
                  tunnel/relay.json. Restart `ligus serve` to connect, then `ligus pair`.",
        synopsis: &["ligus remote enable --relay URL [--reg-token - | @FILE] [--insecure-relay]"],
        flags: &[
            f("--relay URL", "required", "relay base URL (https://)"),
            f("--reg-token - | @FILE", "none", "registration token for a gated relay"),
            f("--insecure-relay", "off", "allow an http:// relay (tests, loopback)"),
        ],
        examples: &["ligus remote enable --relay https://relay.example.com", "ligus remote enable --relay https://relay.example.com --reg-token @./reg-token"],
        see_also: &["ligus remote status", "ligus pair"],
        ..D
    },
    Cmd {
        name: "remote status",
        group: G_PHONE,
        summary: "relay, host id, the running serve's link state, phones",
        purpose: "Print the relay and whether it is enabled, the host id, what the running `ligus serve` last reported \
                  about its relay connection, and the paired phones' notification state. `ligus remote` alone is the \
                  same.",
        synopsis: &["ligus remote [status] [--json]"],
        flags: &[JSON],
        examples: &["ligus remote status", "ligus remote status --json"],
        see_also: &["ligus remote push-test", "ligus help troubleshooting"],
        ..D
    },
    Cmd {
        name: "remote disable",
        group: G_PHONE,
        summary: "stop using the relay; keys and phones are kept",
        purpose: "Mark the relay disabled (keys and phones are kept, so enabling again needs no re-pairing). Restart \
                  `ligus serve`.",
        synopsis: &["ligus remote disable"],
        examples: &["ligus remote disable"],
        see_also: &["ligus remote enable"],
        ..D
    },
    Cmd {
        name: "remote reset",
        group: G_PHONE,
        summary: "new host keys and host id; every phone pairs again",
        purpose: "Generate new host keys (a new host id), forget the paired phones and the live status. Every phone \
                  must pair again; restart `ligus serve`.",
        synopsis: &["ligus remote reset"],
        examples: &["ligus remote reset && ligus pair"],
        see_also: &["ligus remote enable", "ligus pair"],
        ..D
    },
    Cmd {
        name: "remote push-test",
        group: G_PHONE,
        summary: "send a test notification and show each hop",
        purpose: "Ask the running `ligus serve` to send a test notification to the paired phones (or one), and print \
                  each hop: phone opted in, box to relay, and what APNs answered.",
        synopsis: &["ligus remote push-test [--phone PID|NAME]"],
        flags: &[f("--phone PID|NAME", "every paired phone", "one phone")],
        env: &["LIGUS_REMOTE_RELAY"],
        examples: &["ligus remote push-test", "ligus remote push-test --phone \"Ana's iPhone\""],
        see_also: &["ligus remote status", "ligus phones list"],
        ..D
    },
    Cmd {
        name: "phones",
        group: G_PHONE,
        summary: "phones paired with this box: list, revoke, certify",
        purpose: "Manage the phones paired with this box over the relay, and certify a phone's key as one of your \
                  devices without a desktop.",
        synopsis: &[
            "ligus phones [list] [--json]",
            "ligus phones revoke <pid|name> [--keep-token] [--token-file PATH]",
            "ligus phones certify <ligusphone1:code> | --code-file PATH  --recovery-words-file PATH --fingerprint F",
        ],
        flags: &[JSON],
        examples: &["ligus phones", "ligus phones revoke \"Ana's iPhone\""],
        see_also: &["ligus phones list", "ligus phones revoke", "ligus phones certify", "ligus help phone"],
        ..D
    },
    Cmd {
        name: "phones list",
        group: G_PHONE,
        summary: "phones paired over the relay, and revocations in progress",
        purpose: "List each paired phone (pid, name, active or revoked, paired and last-seen dates) and the steps of \
                  each revocation still in progress. `ligus phones` alone is the same.",
        synopsis: &["ligus phones [list] [--json]"],
        flags: &[JSON],
        examples: &["ligus phones list", "ligus phones list --json"],
        see_also: &["ligus phones revoke"],
        ..D
    },
    Cmd {
        name: "phones revoke",
        group: G_PHONE,
        summary: "revoke one phone; also rotates serve.token",
        purpose: "Revoke a phone: a running `ligus serve` ends its sessions and tells the relay within seconds. Unless \
                  --keep-token, also rotates the direct-path bearer token (phones on the direct path pair again).",
        synopsis: &["ligus phones revoke <pid|name> [--keep-token] [--token-file PATH]"],
        flags: &[
            f("--keep-token", "off", "don't rotate the direct-path token"),
            f("--token-file PATH", "LIGUS_SERVE_TOKEN_FILE, else <data dir>/serve.token", "the token file to rotate"),
        ],
        env: &["LIGUS_SERVE_TOKEN_FILE"],
        examples: &["ligus phones revoke 8f3k2…", "ligus phones revoke \"Ana's iPhone\" --keep-token"],
        see_also: &["ligus phones list", "ligus pair"],
        ..D
    },
    Cmd {
        name: "phones certify",
        group: G_PHONE,
        summary: "headless \"Add a phone\": certify a phone's key with your recovery key",
        purpose: "Certify the phone whose code (`ligusphone1:…`, from the app: Settings > This phone > Can't scan? Show \
                  this phone's code) is given as one of this identity's devices, at the phone tier, confirmed with \
                  your recovery key. The fingerprint is required: --fingerprint, or at a terminal it shows the \
                  fingerprint and asks; with neither (stdin not a terminal) it refuses. Never from a box. Sync to \
                  publish, then tap Retry on the phone.",
        synopsis: &["ligus phones certify <ligusphone1:code> | --code-file PATH  --recovery-words-file PATH [--fingerprint XXXX-XXXX-XXXX-XXXX-XXXX]"],
        flags: &[
            f("--code-file PATH", "none", "read the phone's code from a file"),
            f("--recovery-words-file PATH", "required", "your recovery key's 24 words (owner-only file)"),
            f("--fingerprint F", "asked at a terminal; required otherwise", "the fingerprint the phone shows"),
        ],
        conn: true,
        examples: &["ligus phones certify --code-file ./phone-code --recovery-words-file ~/.ligus-words --fingerprint ABCD-EFGH-JKLM-NPQR-STUV && ligus sync"],
        see_also: &["ligus help phone", "ligus recovery-key create"],
        ..D
    },
    // ── Repositories ───────────────────────────────────────────────────
    Cmd {
        name: "repos",
        group: G_REPOS,
        summary: "workspace repositories and this host's checkouts of them",
        purpose: "Workspace repositories (synced; adding, removing and aliases need an Admin with presence) and this \
                  host's checkouts of them (device-local, never synced). Agent turns for a chat with a repository run \
                  only in this host's confirmed checkout.",
        synopsis: &[
            "ligus repos [list] [--json]",
            "ligus repos add <remote|canonical|path> [--name N] [--kind K] [--branch B]",
            "ligus repos remove <repo> | rename <repo> <name> [--branch B]",
            "ligus repos bind|confirm <repo> <path> | unbind <repo> [path] | takeover <path>",
            "ligus repos scan | roots [add|remove <path>]",
            "ligus repos alias [<ssh-host> <host> | <ssh-host> --remove]",
            "ligus repos aliases | repo-alias <repo> [names…] | host-alias <alias> <host> | <alias> --remove",
            "ligus repos accept <alias> [--revoke]",
            "ligus repos chat <chat-id> <repo|none> [--branch B] | coverage on|off|default",
        ],
        flags: &[f("--label NAME", "LIGUS_HOST_LABEL, else $HOSTNAME, else \"this host\"", "this host's label in messages"), JSON],
        env: &["LIGUS_HOST_LABEL", "HOSTNAME", "LIGUS_REPOS_DIR"],
        conn: true,
        examples: &["ligus repos", "ligus repos add git@github.com:acme/web.git --branch main", "ligus repos scan && ligus repos confirm web ~/src/web"],
        see_also: &["ligus help repos", "ligus bind-repo", "ligus forge"],
        ..D
    },
    Cmd {
        name: "repos list",
        group: G_REPOS,
        summary: "repositories, this host's checkouts, the repos dir and bindings",
        purpose: "List the workspace's repositories and this host's checkouts of each (primary, confirmed, or found \
                  and unconfirmed), then the repos dir and the bindings. `ligus repos` alone is the same.",
        synopsis: &["ligus repos [list] [--json]"],
        flags: &[JSON],
        env: &["LIGUS_REPOS_DIR"],
        conn: true,
        examples: &["ligus repos list", "ligus repos list --json"],
        see_also: &["ligus repos add", "ligus repos scan"],
        ..D
    },
    Cmd {
        name: "repos add",
        group: G_REPOS,
        summary: "add a workspace repository (Admin, with presence)",
        purpose: "Add a workspace repository from a remote URL, a canonical name or a local path (a path also binds \
                  it here and takes its default branch from origin/HEAD).",
        synopsis: &["ligus repos add <remote|canonical|path> [--name N] [--kind K] [--branch B]"],
        flags: &[
            f("--name N", "from the remote", "display name"),
            f("--kind K", "guessed from the remote", "code host kind (github, gitlab, forgejo, gitea, …)"),
            f("--branch B", "origin/HEAD of a path, else empty", "default branch"),
        ],
        conn: true,
        examples: &["ligus repos add https://github.com/acme/web --branch main", "ligus repos add ~/src/web && ligus sync"],
        see_also: &["ligus repos list", "ligus repos bind"],
        ..D
    },
    Cmd {
        name: "repos remove",
        group: G_REPOS,
        summary: "remove a workspace repository (Admin, with presence)",
        purpose: "Remove a workspace repository (by id prefix or name).",
        synopsis: &["ligus repos remove <repo>"],
        conn: true,
        examples: &["ligus repos remove web && ligus sync"],
        see_also: &["ligus repos add"],
        ..D
    },
    Cmd {
        name: "repos rename",
        group: G_REPOS,
        summary: "rename a workspace repository (and its default branch)",
        purpose: "Rename a workspace repository; --branch changes its default branch.",
        synopsis: &["ligus repos rename <repo> <name> [--branch B]"],
        flags: &[f("--branch B", "unchanged", "new default branch")],
        conn: true,
        examples: &["ligus repos rename web frontend"],
        see_also: &["ligus repos list"],
        ..D
    },
    Cmd {
        name: "repos bind",
        group: G_REPOS,
        summary: "this host's checkout of a repository",
        purpose: "Record PATH as this host's checkout of the repository (device-local).",
        synopsis: &["ligus repos bind <repo> <path>"],
        conn: true,
        examples: &["ligus repos bind web ~/src/web"],
        see_also: &["ligus repos confirm", "ligus repos unbind"],
        ..D
    },
    Cmd {
        name: "repos confirm",
        group: G_REPOS,
        summary: "confirm a found checkout",
        purpose: "Confirm a checkout `ligus repos scan` found, so agent turns may run in it.",
        synopsis: &["ligus repos confirm <repo> <path>"],
        conn: true,
        examples: &["ligus repos confirm web ~/src/web"],
        see_also: &["ligus repos scan"],
        ..D
    },
    Cmd {
        name: "repos unbind",
        group: G_REPOS,
        summary: "forget this host's checkout of a repository",
        purpose: "Forget this host's checkout (one path, or all of them) of a repository. Files are left alone.",
        synopsis: &["ligus repos unbind <repo> [path]"],
        conn: true,
        examples: &["ligus repos unbind web"],
        see_also: &["ligus repos bind"],
        ..D
    },
    Cmd {
        name: "repos takeover",
        group: G_REPOS,
        summary: "manage a checkout whose managing host is gone",
        purpose: "Make this host the manager of a checkout another host on this machine managed (and is gone).",
        synopsis: &["ligus repos takeover <path>"],
        conn: true,
        examples: &["ligus repos takeover ~/src/web"],
        see_also: &["ligus repos bind"],
        ..D
    },
    Cmd {
        name: "repos scan",
        group: G_REPOS,
        summary: "find checkouts under the repos dir and the scan roots",
        purpose: "Look for checkouts of the workspace's repositories under the repos dir and the scan roots; found ones \
                  need `ligus repos confirm`.",
        synopsis: &["ligus repos scan"],
        env: &["LIGUS_REPOS_DIR"],
        conn: true,
        examples: &["ligus repos roots add ~/src && ligus repos scan"],
        see_also: &["ligus repos roots", "ligus repos confirm"],
        ..D
    },
    Cmd {
        name: "repos roots",
        group: G_REPOS,
        summary: "list, add or remove scan roots",
        purpose: "The folders `ligus repos scan` searches. A root too broad to scan (/, the home dir) is refused.",
        synopsis: &["ligus repos roots [add|remove <path>]"],
        conn: true,
        examples: &["ligus repos roots add ~/src", "ligus repos roots"],
        see_also: &["ligus repos scan"],
        ..D
    },
    Cmd {
        name: "repos alias",
        group: G_REPOS,
        summary: "this host's own SSH host aliases (identity only, never synced)",
        purpose: "Map an SSH host alias (from ~/.ssh/config) to the real host, so this host recognises a checkout's \
                  remote. No arguments lists them.",
        synopsis: &["ligus repos alias", "ligus repos alias <ssh-host> <host>", "ligus repos alias <ssh-host> --remove"],
        flags: &[f("--remove", "off", "remove the alias")],
        conn: true,
        examples: &["ligus repos alias gh-work github.com"],
        see_also: &["ligus repos aliases"],
        ..D
    },
    Cmd {
        name: "repos aliases",
        group: G_REPOS,
        summary: "the workspace's synced aliases and which this host accepted",
        purpose: "List the workspace's synced repository and host aliases, and whether this host uses each.",
        synopsis: &["ligus repos aliases"],
        conn: true,
        examples: &["ligus repos aliases"],
        see_also: &["ligus repos accept", "ligus repos repo-alias", "ligus repos host-alias"],
        ..D
    },
    Cmd {
        name: "repos repo-alias",
        group: G_REPOS,
        summary: "replace a repository's synced aliases (Admin, with presence)",
        purpose: "Replace a repository's synced aliases (names only); no names clears them. Each host uses them after \
                  `ligus repos accept`.",
        synopsis: &["ligus repos repo-alias <repo> [names…]"],
        conn: true,
        examples: &["ligus repos repo-alias web acme/web-old"],
        see_also: &["ligus repos accept"],
        ..D
    },
    Cmd {
        name: "repos host-alias",
        group: G_REPOS,
        summary: "a synced host alias (Admin, with presence)",
        purpose: "Set or remove a workspace-synced host alias (identity only). Each host uses it after `ligus repos \
                  accept`.",
        synopsis: &["ligus repos host-alias <alias-host> <repository-host>", "ligus repos host-alias <alias-host> --remove"],
        flags: &[f("--remove", "off", "remove the alias")],
        conn: true,
        examples: &["ligus repos host-alias git.internal github.com"],
        see_also: &["ligus repos accept"],
        ..D
    },
    Cmd {
        name: "repos accept",
        group: G_REPOS,
        summary: "use a synced alias on this host (device-local)",
        purpose: "Use (or with --revoke stop using) a synced alias for identity on this host. Git still connects where \
                  it did.",
        synopsis: &["ligus repos accept <alias> [--revoke]"],
        flags: &[f("--revoke", "off", "stop using it")],
        conn: true,
        examples: &["ligus repos accept git.internal"],
        see_also: &["ligus repos aliases"],
        ..D
    },
    Cmd {
        name: "repos chat",
        group: G_REPOS,
        summary: "set a chat's repository",
        purpose: "Set (or with `none` clear) the repository a chat works in, optionally on a branch.",
        synopsis: &["ligus repos chat <chat-id> <repo|none> [--branch B]"],
        flags: &[f("--branch B", "the repository's default", "the chat's base branch")],
        conn: true,
        examples: &["ligus repos chat 6f1c2a9b-… web --branch main && ligus sync"],
        see_also: &["ligus repos list"],
        ..D
    },
    Cmd {
        name: "repos coverage",
        group: G_REPOS,
        summary: "whether this host tells members which repositories it has",
        purpose: "Turn on or off (or back to the default) this host's reports of which repositories it has checked out.",
        synopsis: &["ligus repos coverage on|off|default"],
        conn: true,
        examples: &["ligus repos coverage off"],
        see_also: &["ligus repos list"],
        ..D
    },
    Cmd {
        name: "repo",
        group: G_REPOS,
        summary: "scratch repositories: start one for a chat, keep it",
        purpose: "Scratch repositories: an empty repository (one empty commit on main) this host creates under its \
                  scratch root for a chat to work in, with the chat's branch, proposals, approvals and the sandbox as \
                  for any checkout. Unkept ones are removed after a quiet spell. The other repository commands are \
                  `ligus repos`.",
        synopsis: &["ligus repo scratch [--chat CHAT-ID] [--name N] [--workspace] [--json]", "ligus repo keep <scratch> [--to PATH] [--name N] [--json]"],
        flags: &[f("--label NAME", "LIGUS_HOST_LABEL, else $HOSTNAME, else \"this host\"", "this host's label in messages"), JSON],
        env: &["LIGUS_HOST_LABEL", "HOSTNAME", "LIGUS_SCRATCH_DIR", "LIGUS_REPOS_DIR"],
        conn: true,
        examples: &["ligus repo scratch --chat 6f1c2a9b-…", "ligus repo scratch --list", "ligus repo keep 1a2b3c4d --to ~/src/parser"],
        see_also: &["ligus repo scratch", "ligus repo keep", "ligus help repos"],
        ..D
    },
    Cmd {
        name: "repo scratch",
        group: G_REPOS,
        summary: "create, list or remove scratch repositories; set the cleanup",
        purpose: "Create an empty repository under this host's scratch root (LIGUS_SCRATCH_DIR, else <data dir>/scratch, \
                  or --set-root; never inside another repository or the desktop app's data folder) and, with --chat, bind \
                  that chat to it here. --workspace (a person's own device, Admin) also adds it to the workspace flagged \
                  scratch and sets it as the chat's workspace repository. Limits: 16 per host, 8 per member by default. \
                  The cleanup removes an unkept one after 14 days without activity (a turn, a commit, a bind), with a \
                  notice in its chats 2 days before; it only ever deletes a folder this host created and recorded, \
                  directly under the scratch root, not a symlink. Its chats are left with no repository; their proposals \
                  and diffs stay.",
        synopsis: &[
            "ligus repo scratch [--chat CHAT-ID] [--name N] [--workspace] [--json]",
            "ligus repo scratch --list [--json] | --remove <scratch> | --sweep [--json]",
            "ligus repo scratch --set-root PATH | --reset-root | --cleanup-days N",
        ],
        flags: &[
            f("--chat CHAT-ID", "none", "bind this chat to the new scratch repository on this host"),
            f("--name N", "scratch-<id>", "its display name (and folder slug)"),
            f("--workspace", "off", "also add it to the workspace, flagged scratch (Admin, with presence)"),
            f("--list", "off", "list this host's scratch repositories and settings"),
            f("--remove", "off", "remove the named scratch repository now (its chats get no repository and a note)"),
            f("--sweep", "off", "run the cleanup once now (ligus serve runs it hourly)"),
            f("--set-root PATH", "<data dir>/scratch", "where new scratch repositories go"),
            f("--reset-root", "off", "back to the default scratch root"),
            f("--cleanup-days N", "14", "days without activity before removal; 0 turns the cleanup off"),
            f("--label NAME", "LIGUS_HOST_LABEL, else $HOSTNAME", "this host's label in messages"),
            JSON,
        ],
        env: &["LIGUS_SCRATCH_DIR", "LIGUS_REPOS_DIR", "LIGUS_HOST_LABEL"],
        conn: true,
        examples: &["ligus repo scratch --chat 6f1c2a9b-… --name \"Parser spike\"", "ligus repo scratch --list --json", "ligus repo scratch --cleanup-days 0"],
        see_also: &["ligus repo keep", "ligus repos", "ligus bind-repo"],
        ..D
    },
    Cmd {
        name: "repo keep",
        group: G_REPOS,
        summary: "keep a scratch repository: move it out of the scratch root",
        purpose: "Keep a scratch repository as an ordinary one: move it to --to (default: the repos dir, under its name), \
                  repair its worktrees, and point its chats there. It is never removed by the cleanup after that. A \
                  workspace-registered one also loses its scratch tag (Admin). Publishing to a code host is the phone's \
                  or the desktop's explicit action, with a confirmation naming the account and repository.",
        synopsis: &["ligus repo keep <scratch> [--to PATH] [--name N] [--json]"],
        flags: &[
            f("--to PATH", "<repos dir>/<name>", "where to keep it (must not exist; outside the scratch root and other repositories; the same disk)"),
            f("--name N", "its scratch name", "the kept repository's name"),
            JSON,
        ],
        env: &["LIGUS_REPOS_DIR"],
        conn: true,
        examples: &["ligus repo keep 1a2b3c4d", "ligus repo keep \"Parser spike\" --to ~/src/parser"],
        see_also: &["ligus repo scratch", "ligus repos"],
        ..D
    },
    Cmd {
        name: "bind-repo",
        group: G_REPOS,
        summary: "bind a checkout for agent file tools (legacy, per box)",
        purpose: "Bind a checkout (a name under the repos dir, or an absolute path) for agent file tools, for the \
                  workspace or one chat, in <data dir>/repo-bindings.toml. A chat with a workspace repository uses \
                  `ligus repos` instead.",
        synopsis: &["ligus bind-repo <name|path> [--chat CHAT-ID]", "ligus bind-repo --clear [--chat CHAT-ID]"],
        flags: &[
            f("--chat CHAT-ID", "the workspace default", "bind for one chat"),
            f("--clear", "off", "remove the binding"),
        ],
        env: &["LIGUS_REPOS_DIR"],
        examples: &["ligus bind-repo ~/src/web", "ligus bind-repo --clear --chat 6f1c2a9b-…"],
        see_also: &["ligus repos", "ligus help repos"],
        ..D
    },
    Cmd {
        name: "forge",
        group: G_REPOS,
        summary: "code-host connections (tokens on stdin, kept in the secret store)",
        purpose: "This device's code-host connections (GitHub, GitLab, Forgejo, Gitea, Codeberg). Tokens are read from \
                  stdin, never the command line, and kept in the OS secret store.",
        synopsis: &[
            "ligus forge [list]",
            "ligus forge add <github|gitlab|forgejo|gitea|codeberg> <https://base> [--auth token|oauth|gh|none] [--client-id ID] [--internal]",
            "ligus forge token <id> <read|write>        (token on stdin)",
            "ligus forge login|test|remove <id>",
        ],
        flags: &[
            f("--auth token|oauth|gh|none", "gh for GitHub, else token", "add: how this device authenticates"),
            f("--client-id ID", "the built-in app", "add: OAuth client id for `forge login`"),
            f("--internal", "off", "add: allow a loopback, private or link-local server"),
        ],
        env: &["LIGUS_GITHUB_APP_CLIENT_ID", "LIGUS_GITLAB_CLIENT_ID"],
        examples: &["ligus forge add gitlab https://gitlab.example.com", "printf %s \"$TOKEN\" | ligus forge token gitlab-gitlab.example.com write"],
        see_also: &["ligus forge add", "ligus forge token", "ligus forge login", "ligus forge test", "ligus forge remove", "ligus help repos"],
        ..D
    },
    Cmd {
        name: "forge list",
        group: G_REPOS,
        summary: "connections and which tokens are present",
        purpose: "List each connection's id, server, auth source, tokens present and login.",
        synopsis: &["ligus forge [list]"],
        examples: &["ligus forge list"],
        see_also: &["ligus forge add"],
        ..D
    },
    Cmd {
        name: "forge add",
        group: G_REPOS,
        summary: "add a code-host connection",
        purpose: "Add (or replace) a connection. A loopback, private or link-local address is refused without \
                  --internal.",
        synopsis: &["ligus forge add <github|gitlab|forgejo|gitea|codeberg> <https://base> [--auth token|oauth|gh|none] [--client-id ID] [--internal]"],
        flags: &[
            f("--auth token|oauth|gh|none", "gh for GitHub, else token", "how this device authenticates"),
            f("--client-id ID", "the built-in app", "OAuth client id for `forge login`"),
            f("--internal", "off", "the server is on this device's network"),
        ],
        examples: &["ligus forge add github https://github.com", "ligus forge add forgejo https://git.lan --internal"],
        see_also: &["ligus forge token", "ligus forge login"],
        ..D
    },
    Cmd {
        name: "forge token",
        group: G_REPOS,
        summary: "store a read or write token, read from stdin",
        purpose: "Read one token line from stdin and keep it in the OS secret store. At a terminal it asks for a paste; \
                  piped, it just reads.",
        synopsis: &["ligus forge token <id> <read|write>"],
        examples: &["printf %s \"$TOKEN\" | ligus forge token gitlab-gitlab.example.com write"],
        see_also: &["ligus forge test"],
        ..D
    },
    Cmd {
        name: "forge login",
        group: G_REPOS,
        summary: "device sign-in (GitHub App, GitLab app)",
        purpose: "Print a URL and a code to approve in a browser, then keep the resulting token.",
        synopsis: &["ligus forge login <id>"],
        env: &["LIGUS_GITHUB_APP_CLIENT_ID", "LIGUS_GITLAB_CLIENT_ID"],
        examples: &["ligus forge login github-github.com"],
        see_also: &["ligus forge test"],
        ..D
    },
    Cmd {
        name: "forge test",
        group: G_REPOS,
        summary: "check a connection's credentials",
        purpose: "Ask the code host who this connection is signed in as.",
        synopsis: &["ligus forge test <id>"],
        examples: &["ligus forge test github-github.com"],
        see_also: &["ligus forge list"],
        ..D
    },
    Cmd {
        name: "forge remove",
        group: G_REPOS,
        summary: "remove a connection and its tokens",
        purpose: "Remove a connection and delete its tokens from the secret store.",
        synopsis: &["ligus forge remove <id>"],
        examples: &["ligus forge remove gitlab-gitlab.example.com"],
        see_also: &["ligus forge list"],
        ..D
    },
    Cmd {
        name: "forge git-credential",
        group: G_REPOS,
        summary: "git credential helper (git runs it; not for people)",
        purpose: "Git's credential-helper protocol on stdin/stdout, for Ligus's own pushes: answers `get` with the \
                  write token, only for that connection's own https host.",
        synopsis: &["ligus forge git-credential <id> <get|store|erase>"],
        examples: &["git config credential.https://gitlab.example.com.helper '!ligus forge git-credential gitlab-gitlab.example.com'"],
        see_also: &["ligus forge token"],
        ..D
    },
    // ── Memory, facts, dashboards ──────────────────────────────────────
    Cmd {
        name: "memory",
        group: G_DATA,
        summary: "workspace memory (shared notes)",
        purpose: "Workspace memory from the command line. Writes are this identity's: a person's identity writes and \
                  decides; a box can only read and propose. A body comes from --body or stdin.",
        synopsis: &[
            "ligus memory [list] [--tag a,b] [--json]",
            "ligus memory search <words…> [--tag a,b] [--json] | show <id> [--json]",
            "ligus memory add <title> [--body TEXT] [--tag a,b]",
            "ligus memory edit <id> [--title T] [--body TEXT] [--tag a,b] [--base REV]",
            "ligus memory rm <id> [--base REV]",
            "ligus memory proposals [--json] | accept <proposal-id> | reject <proposal-id> [--note TEXT]",
        ],
        flags: &[f("--tag a,b", "none", "filter (list, search) or set (add, edit) tags"), JSON],
        conn: true,
        examples: &["ligus memory search relay --json", "echo \"The relay listens on 8443.\" | ligus memory add Relay ports --tag infra"],
        see_also: &["ligus memory list", "ligus memory add", "ligus memory proposals"],
        ..D
    },
    Cmd {
        name: "memory list",
        group: G_DATA,
        summary: "the live notes, newest first",
        purpose: "List the live notes. `ligus memory` alone is the same.",
        synopsis: &["ligus memory [list] [--tag a,b] [--json]"],
        flags: &[f("--tag a,b", "none", "only notes with these tags"), JSON],
        conn: true,
        examples: &["ligus memory list --tag infra"],
        see_also: &["ligus memory search"],
        ..D
    },
    Cmd {
        name: "memory search",
        group: G_DATA,
        summary: "notes matching every word, best first",
        purpose: "Notes matching every word (title, body or tags), best first.",
        synopsis: &["ligus memory search <words…> [--tag a,b] [--json]"],
        flags: &[f("--tag a,b", "none", "only notes with these tags"), JSON],
        conn: true,
        examples: &["ligus memory search relay port"],
        see_also: &["ligus memory show"],
        ..D
    },
    Cmd {
        name: "memory show",
        group: G_DATA,
        summary: "one note with its version, author, tags and conflicts",
        purpose: "Print one note: its version (rev), author, source chat, tags, body and any conflicts.",
        synopsis: &["ligus memory show <id> [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus memory show 0931232f-… --json"],
        see_also: &["ligus memory edit"],
        ..D
    },
    Cmd {
        name: "memory add",
        group: G_DATA,
        summary: "a new note (body from --body or stdin)",
        purpose: "Write a new note; prints its id and rev. Without --body the body is read from stdin (at a terminal: \
                  type it, then Ctrl-D).",
        synopsis: &["ligus memory add <title> [--body TEXT] [--tag a,b]"],
        flags: &[f("--body TEXT", "stdin", "the note's body"), f("--tag a,b", "none", "tags")],
        conn: true,
        examples: &["ligus memory add Relay ports --body \"8443\" --tag infra", "cat notes.md | ligus memory add Runbook"],
        see_also: &["ligus memory edit"],
        ..D
    },
    Cmd {
        name: "memory edit",
        group: G_DATA,
        summary: "a new version of a note",
        purpose: "Write a new version. From the current version it also settles the note's conflicts; --base edits \
                  from an older one (kept as a conflict). Body from --body or stdin.",
        synopsis: &["ligus memory edit <id> [--title T] [--body TEXT] [--tag a,b] [--base REV]"],
        flags: &[
            f("--title T", "unchanged", "new title"),
            f("--body TEXT", "stdin", "new body"),
            f("--tag a,b", "unchanged", "new tags"),
            f("--base REV", "the current version", "edit from this version"),
        ],
        conn: true,
        examples: &["ligus memory edit 0931232f-… --body \"8443 and 8444\""],
        see_also: &["ligus memory show"],
        ..D
    },
    Cmd {
        name: "memory rm",
        group: G_DATA,
        summary: "remove a note (history stays in the log)",
        purpose: "Remove a note; its history stays in the workspace log.",
        synopsis: &["ligus memory rm <id> [--base REV]"],
        flags: &[f("--base REV", "the current version", "remove from this version")],
        conn: true,
        examples: &["ligus memory rm 0931232f-…"],
        see_also: &["ligus memory list"],
        ..D
    },
    Cmd {
        name: "memory proposals",
        group: G_DATA,
        summary: "proposed changes, open first",
        purpose: "List proposed memory changes (from agents and boxes), open first.",
        synopsis: &["ligus memory proposals [--json]"],
        flags: &[JSON],
        conn: true,
        examples: &["ligus memory proposals --json"],
        see_also: &["ligus memory accept", "ligus memory reject"],
        ..D
    },
    Cmd {
        name: "memory accept",
        group: G_DATA,
        summary: "accept a memory proposal",
        purpose: "Accept a proposed memory change.",
        synopsis: &["ligus memory accept <proposal-id>"],
        conn: true,
        examples: &["ligus memory accept 5d0e…"],
        see_also: &["ligus memory proposals"],
        ..D
    },
    Cmd {
        name: "memory reject",
        group: G_DATA,
        summary: "reject a memory proposal",
        purpose: "Reject a proposed memory change, with an optional note.",
        synopsis: &["ligus memory reject <proposal-id> [--note TEXT]"],
        flags: &[f("--note TEXT", "empty", "why")],
        conn: true,
        examples: &["ligus memory reject 5d0e… --note \"out of date\""],
        see_also: &["ligus memory proposals"],
        ..D
    },
    Cmd {
        name: "facts",
        group: G_DATA,
        summary: "content-free facts for observability tools",
        purpose: "Export this host's content-free facts (ligus.facts.v1) for external observability tools.",
        synopsis: &["ligus facts export --jsonl [--since CURSOR]"],
        examples: &["ligus facts export --jsonl"],
        see_also: &["ligus facts export"],
        ..D
    },
    Cmd {
        name: "facts export",
        group: G_DATA,
        summary: "facts as JSON lines; the last line's cursor is the next --since",
        purpose: "Print facts (measurements, guard signals, state facts) as JSON lines. Read-only: it never creates a \
                  data dir, an identity or a database (a missing ligus.db is an error).",
        synopsis: &["ligus facts export --jsonl [--since CURSOR | UNIX-MS]"],
        flags: &[
            f("--jsonl", "required", "JSON lines output"),
            f("--since CURSOR", "0 (everything)", "the cursor from the previous export's last line, or unix milliseconds"),
        ],
        examples: &["ligus facts export --jsonl > facts.jsonl", "ligus facts export --jsonl --since \"$(tail -1 facts.jsonl | jq -r '.attributes[\"ligus.cursor\"]')\""],
        see_also: &["ligus dashboard"],
        ..D
    },
    Cmd {
        name: "dashboard",
        group: G_DATA,
        summary: "the serve dashboard config: check, default, metrics",
        purpose: "The dashboard `ligus serve` shows the phone, from dashboard.toml.",
        synopsis: &["ligus dashboard check [--file PATH]", "ligus dashboard default", "ligus dashboard metrics"],
        env: &["LIGUS_DASHBOARD_FILE", "LIGUS_PROMETHEUS_URL"],
        examples: &["ligus dashboard default > ~/.ligus/dashboard.toml && ligus dashboard check"],
        see_also: &["ligus dashboard check", "ligus dashboard default", "ligus dashboard metrics"],
        ..D
    },
    Cmd {
        name: "dashboard check",
        group: G_DATA,
        summary: "validate dashboard.toml and evaluate it once",
        purpose: "Validate the dashboard config and evaluate every panel once; fails when the config is invalid or a \
                  panel errors.",
        synopsis: &["ligus dashboard check [--file PATH]"],
        flags: &[f("--file PATH", "LIGUS_DASHBOARD_FILE, else <data dir>/dashboard.toml", "the config")],
        env: &["LIGUS_DASHBOARD_FILE", "LIGUS_PROMETHEUS_URL", "LIGUS_MCP_CONFIG", "LIGUS_LAB_TEMPS"],
        examples: &["ligus dashboard check", "ligus dashboard check --file ./dashboard.toml"],
        see_also: &["ligus dashboard default"],
        ..D
    },
    Cmd {
        name: "dashboard default",
        group: G_DATA,
        summary: "print the generated default dashboard as TOML",
        purpose: "Print the default dashboard (with a lab section when LIGUS_PROMETHEUS_URL is set).",
        synopsis: &["ligus dashboard default"],
        env: &["LIGUS_PROMETHEUS_URL", "LIGUS_LAB_TEMPS"],
        examples: &["ligus dashboard default > dashboard.toml"],
        see_also: &["ligus dashboard check"],
        ..D
    },
    Cmd {
        name: "dashboard metrics",
        group: G_DATA,
        summary: "list the built-in metrics panels can show",
        purpose: "List the built-in Ligus metrics a panel can show.",
        synopsis: &["ligus dashboard metrics"],
        examples: &["ligus dashboard metrics"],
        see_also: &["ligus dashboard default"],
        ..D
    },
    // ── Checks ─────────────────────────────────────────────────────────
    Cmd {
        name: "doctor",
        group: G_CHECK,
        summary: "check everything on this host; --json prints ligus.checks.v1",
        purpose: "Run every check this host can make and say how to fix what fails: the agent sandbox tier (seatbelt, \
                  bwrap, landlock or unconfined) and what it leaves open, whether the process is dumpable, the \
                  workspace's signed sandbox grants, the secret store, the workspace connection, the relay (one \
                  request, 10 s at most), the held events and this host's model gateways. --sandbox or --gateways \
                  narrows it to those checks; a preflight flag (--require-confined, --expect-tier, --turn-test) \
                  narrows it to the sandbox and makes it exit non-zero, for init containers and CI. With --json it \
                  prints one ligus.checks.v1 document (ligus help checks). --sandbox adds what an agent turn may \
                  read and write here (when the checks pass).",
        synopsis: &[
            "ligus doctor [--json]",
            "ligus doctor [--require-confined] [--expect-tier TIER] [--turn-test] [--json]",
            "ligus doctor --sandbox [--runtime NAME] [--agent AGENT-ID] [--json]",
            "ligus doctor --gateways [--gateway NAME] [--model ID] [--json]",
        ],
        flags: &[
            f("--require-confined", "off", "fail without an agent sandbox"),
            f("--expect-tier TIER", "none", "fail unless the tier is TIER: seatbelt, bwrap, landlock, unconfined"),
            f("--turn-test", "off", "run one real headless agent turn against a stub model; fail if it isn't confined"),
            f("--legacy-exit-codes", "off", "exit 3, 4 or 5 for those failures, as before 0.2 (kept for one release)"),
            f("--sandbox", "off", "only the sandbox checks, then the per-runtime read/write policy and the grants in force"),
            f("--runtime NAME", "every runtime", "with --sandbox: claude, codex, copilot, agy, aider, pi, hermes or commands"),
            f("--agent AGENT-ID", "none", "with --sandbox: include that agent's signed grants"),
            f("--gateways", "off", "only the model gateway checks (the same as ligus gateway-check)"),
            f("--gateway NAME", "every gateway", "check one model gateway (implies --gateways)"),
            f("--model ID", "ligus-gateway-check", "the model id the gateway checks ask for"),
            JSON,
        ],
        env: &[
            "LIGUS_SANDBOX",
            "LIGUS_SANDBOX_GRANTS",
            "LIGUS_SANDBOX_READ_ALLOW",
            "LIGUS_SANDBOX_WRITE_ALLOW",
            "LIGUS_SECRETS_DIR",
            "LIGUS_MODEL_GATEWAYS_FILE",
        ],
        conn: true,
        examples: &["ligus doctor --json", "ligus doctor --require-confined --turn-test", "ligus doctor --sandbox --runtime claude"],
        exit: &[
            (3, "only with --legacy-exit-codes: --require-confined or --turn-test without an agent sandbox"),
            (4, "only with --legacy-exit-codes: --expect-tier found another tier"),
            (5, "only with --legacy-exit-codes: --turn-test failed or could not run"),
        ],
        see_also: &[
            "ligus help checks",
            "ligus help security",
            "ligus help troubleshooting",
            "ligus gateway-check",
            "langstroth-relay check --url <relay> (on the relay host)",
            "deploy/local/ligus-local preflight, deploy/kubernetes/ligus-k8s preflight --probe (the same sandbox check)",
            "desktop: Settings → Sandbox (the same policy and grants)",
        ],
        ..D
    },
    Cmd {
        name: "gateway-check",
        group: G_CHECK,
        summary: "prove this host's model gateways end to end",
        purpose: "Prove the model gateways from LIGUS_MODEL_GATEWAYS_FILE work the two ways agent turns use them: a \
                  hosted-API turn, and a sandboxed CLI child through the egress proxy's route (which must not see the \
                  credential). Prints nothing secret.",
        synopsis: &["ligus gateway-check [--gateway NAME] [--model ID] [--legacy-exit-codes]"],
        flags: &[
            f("--gateway NAME", "every gateway", "check one gateway"),
            f("--model ID", "ligus-gateway-check", "model id to ask for"),
            f("--legacy-exit-codes", "off", "exit 6 when a check fails, as before 0.2 (kept for one release)"),
        ],
        env: &["LIGUS_MODEL_GATEWAYS_FILE", "LIGUS_MODEL_GATEWAYS_EXCLUSIVE", "LIGUS_MODEL_GATEWAY_ATTRIBUTION_KEY_FILE", "LIGUS_WORKSPACE"],
        examples: &["ligus gateway-check", "ligus gateway-check --gateway litellm --model gpt-4o-mini"],
        exit: &[(6, "only with --legacy-exit-codes: a check failed (otherwise 1)")],
        see_also: &["ligus doctor"],
        ..D
    },
];

/// One environment variable for `ligus help env`.
pub struct EnvVar {
    pub name: &'static str,
    pub default: &'static str,
    pub about: &'static str,
}

const fn e(name: &'static str, default: &'static str, about: &'static str) -> EnvVar {
    EnvVar { name, default, about }
}

/// `ligus help env`, by section. Booleans accept 1/true/on/yes and
/// 0/false/off/no; a blank value is unset.
pub const ENV: &[(&str, &[EnvVar])] = &[
    (
        "Data dir, connection and secrets",
        &[
            e("LIGUS_DATA_DIR", "~/.ligus (./ligus-data without HOME)", "this device's data dir: identity, keys, ligus.db, config.toml, serve and tunnel state"),
            e("LIGUS_RELAY_URL", "config.toml", "relay base URL"),
            e("LIGUS_RELAY_ACCESS_TOKEN", "secret store, or a langstroth-auth session", "relay access token"),
            e("LIGUS_RELAY_ACCESS_TOKEN_FILE", "unset", "a file holding LIGUS_RELAY_ACCESS_TOKEN (not both)"),
            e("LIGUS_WORKSPACE", "config.toml, else \"default\"", "relay room / workspace"),
            e("LIGUS_WORKSPACE_KEY", "secret store", "end-to-end workspace key"),
            e("LIGUS_WORKSPACE_KEY_FILE", "unset", "a file holding LIGUS_WORKSPACE_KEY (not both)"),
            e("LIGUS_SECRET_BACKEND", "the OS store", "where secrets live: keychain, secret-service, systemd-creds, k8s or file"),
            e("LIGUS_SECRETS_DIR", "/var/run/secrets/ligus with the k8s backend", "the k8s backend's mounted Secret dir"),
            e("LIGUS_WS_SECRET_<ref>", "unset", "the key of a workspace runtime with secretRef <ref>"),
            e("LIGUS_WS_SECRET_DIR", "unset", "a dir of files, each becoming LIGUS_WS_SECRET_<file name>"),
            e("LIGUS_ADMISSION_FILE", "unset", "a file holding a box admission code, redeemed by serve and worker at start-up"),
            e("LIGUS_IDENTITY_MODE", "ignored", "ignored (logs a warning)"),
        ],
    ),
    (
        "serve",
        &[
            e("LIGUS_SERVE_LISTEN", "127.0.0.1:8787", "API address; loopback, LAN, link-local, Tailscale or ULA only"),
            e("LIGUS_SERVE_ALLOW_ANY_BIND", "off", "allow any address, 0.0.0.0 included"),
            e("LIGUS_SERVE_TOKEN_FILE", "<data dir>/serve.token", "the direct-path bearer token file"),
            e("LIGUS_SERVE_PUBLIC_URL", "http://<listen>", "URL in the pairing link"),
            e("LIGUS_SERVE_LABEL", "the host name, else \"home host\"", "the box's host label; also the name a new box identity starts with"),
            e("LIGUS_SERVE_NAME", "unset", "display name the box joins under"),
            e("LIGUS_SERVE_DEFAULT_AGENT", "unset", "agent that gets phone messages mentioning nobody"),
            e("LIGUS_SERVE_PALETTE", "unset", "theme palette for the phone's Match host"),
            e("LIGUS_SERVE_APPEARANCE", "unset", "theme mode for the phone's Match host"),
            e("LIGUS_DESKTOP_DATA_DIR", "the desktop's app-data dir", "where to read the desktop's theme on this machine"),
            e("LIGUS_SERVE_OWNER", "removed", "refused with an error: a box is never a person's device"),
            e("LIGUS_REMOTE_RELAY", "tunnel/relay.json", "relay for the phone tunnel; off disables it for this run"),
            e("LIGUS_REMOTE_REG_TOKEN", "relay.json", "registration token for a gated relay"),
            e("LIGUS_REMOTE_REG_TOKEN_FILE", "unset", "a file holding LIGUS_REMOTE_REG_TOKEN"),
            e("LIGUS_REMOTE_INSECURE_RELAY", "off", "allow an http:// relay"),
            e("LIGUS_NTFY_URL", "unset", "self-hosted ntfy server (https://; with the topic)"),
            e("LIGUS_NTFY_TOPIC", "unset", "ntfy topic"),
            e("LIGUS_NTFY_TOKEN", "unset", "ntfy access token"),
            e("LIGUS_NTFY_TOKEN_FILE", "unset", "a file holding LIGUS_NTFY_TOKEN"),
            e("LIGUS_NTFY_ALLOW_HTTP", "off", "accept an http:// ntfy URL"),
            e("LIGUS_NTFY_TITLES", "off", "send who did what and the chat title (never message text)"),
            e("LIGUS_DASHBOARD_FILE", "<data dir>/dashboard.toml", "dashboard config"),
            e("LIGUS_PROMETHEUS_URL", "http://127.0.0.1:9090", "Prometheus for dashboard panels; set: a lab section is generated"),
            e("LIGUS_LAB_TEMPS", "unset", "temperature queries for the generated lab section"),
            e("LIGUS_REPOS_DIR", "<data dir>/repos", "checkouts agents and the phone may bind and clone into"),
            e("LIGUS_SCRATCH_DIR", "<data dir>/scratch", "where this host creates scratch repositories (ligus repo scratch)"),
            e("LIGUS_SCRATCH_SWEEP_SECS", "3600", "how often ligus serve runs the scratch cleanup (at least 30)"),
            e("LIGUS_REPOS_MAX_CLONE_MB", "2048", "clone size cap"),
            e("LIGUS_REPOS_CLONE_TIMEOUT_SECS", "900", "clone and fetch timeout"),
            e("LIGUS_AUTH_ADMIN_URL", "unset", "langstroth-auth admin API, to revoke a phone's sign-in (with the two below)"),
            e("LIGUS_AUTH_ADMIN_TENANT", "unset", "its tenant"),
            e("LIGUS_AUTH_ADMIN_TOKEN_FILE", "unset", "an owner-only file holding that tenant's device-revocation token"),
        ],
    ),
    (
        "worker and agent turns",
        &[
            e("LIGUS_WORKER_HEALTH_LISTEN", "unset", "worker: serve /v1/health and /v1/ready here"),
            e("LIGUS_WORKER_PERMISSION_MODE", "read-only", "acceptEdits, runCommands or bypassPermissions raises worker CLI turns"),
            e("LIGUS_WORKER_TOOLS", "on", "the agent tool loop for local-capable HTTP runtimes on a worker"),
            e("LIGUS_RUNTIMES_FILE", "<data dir>/runtimes.toml", "this host's local runtimes (e.g. a mounted ConfigMap)"),
            e("LIGUS_BUILTIN_TOOLS", "on", "built-in file tools (read, list, search; writes in an isolated worktree)"),
            e("LIGUS_BUILTIN_RUN_COMMAND", "off", "also offer run_command, inside the turn's worktree"),
            e("LIGUS_ALLOW_REMOTE_FILE_TOOLS", "off", "file tools even when the model endpoint isn't local or private"),
            e("LIGUS_MCP_AFTER_READS", "off", "turns OFF the guard refusing MCP calls after a turn read project files"),
            e("LIGUS_MCP_CONFIG", "unset", "TOML file of MCP servers for headless turns and dashboard panels (read with <data dir>/mcp-servers.toml, which `ligus setup tool` writes)"),
            e("LIGUS_RUNTIME_TEST_TIMEOUT_SECS", "45", "seconds `ligus setup agent --test-turn` waits for the test reply"),
            e("LIGUS_MCP_MIN_ROLE", "admin (write tier) / contributor (read-only)", "minimum author role for a turn at its configured tier"),
            e("LIGUS_TURN_TIMEOUT_SECS", "3600", "total backstop for one turn"),
            e("LIGUS_TURN_IDLE_TIMEOUT_SECS", "300", "how long a streaming model may stay silent"),
            e("LIGUS_AGENT_TIMEOUT_SECS", "300", "wall-clock budget of one subprocess agent turn"),
            e("LIGUS_MODEL_GATEWAYS_FILE", "unset", "the operator's model gateways; an unusable file stops the host (exit 2)"),
            e("LIGUS_MODEL_GATEWAYS_EXCLUSIVE", "on when gateways are listed", "0: also run turns that don't use a listed gateway"),
            e("LIGUS_MODEL_GATEWAY_ATTRIBUTION_KEY_FILE", "generated in the data dir", "attribution key file (32+ bytes, the same on every host of a workspace)"),
            e("LIGUS_MODEL_PRICES", "unset", "prefix=in/out,… USD per million tokens, for spend budgets"),
            e("LIGUS_MCP_MAX_SPEND_USD", "unset", "spend per turn"),
        ],
    ),
    (
        "Agent sandbox",
        &[
            e("LIGUS_REQUIRE_SANDBOX", "unset", "1: serve and worker exit 3 instead of starting without an agent sandbox"),
            e("LIGUS_SANDBOX", "on", "off runs agents unconfined (write turns refused); Linux: landlock or bwrap pins a tier"),
            e("LIGUS_SANDBOX_GRANTS", "Owner's hosts only", "apply or ignore the workspace's signed sandbox grants"),
            e("LIGUS_SANDBOX_READ_ALLOW", "unset", "paths under ~ agents may read after all (comma-separated)"),
            e("LIGUS_SANDBOX_WRITE_ALLOW", "unset", "extra paths agents may write (comma-separated)"),
            e("LIGUS_SANDBOX_EGRESS_ALLOW", "unset", "host[:port] agents may reach through the egress proxy"),
            e("LIGUS_SANDBOX_EGRESS_PRESETS", "unset", "package-registries, claude-connectors"),
            e("LIGUS_SANDBOX_UNIX_SOCKETS", "unset", "Linux: allow lets agents create unix sockets"),
            e("LIGUS_CLI_HOME", "per-turn", "shared lets CLI agents write their real config dirs"),
        ],
    ),
    (
        "ligus agent (development responder)",
        &[
            e("LIGUS_ALLOW_PERSONAL_AGENT", "unset", "must be set for `ligus agent` to run"),
            e("LIGUS_AGENT_OPEN_TO_MEMBERS", "unset", "1: answer every member, not only the operator"),
            e("LIGUS_RUNTIME", "unset", "workspace runtime id (as --runtime)"),
            e("LIGUS_PROVIDER", "anthropic", "anthropic, openai, openrouter or ollama without a workspace runtime"),
            e("LIGUS_MODEL", "claude-sonnet-4-5", "model for that provider"),
            e("LIGUS_MODEL_BASE_URL", "http://localhost:11434/v1/chat/completions", "Ollama endpoint"),
            e("LIGUS_PROVIDER_API_KEY", "unset", "provider key overriding ANTHROPIC_API_KEY / OPENAI_API_KEY / OPENROUTER_API_KEY"),
        ],
    ),
    (
        "Other commands",
        &[
            e("LIGUS_HOST_LABEL", "$HOSTNAME, else \"this host\"", "ligus repos: this host's label"),
            e("LIGUS_APP", "unset", "ligus app (macOS): the Ligus.app bundle to open, wherever it is"),
            e("LIGUS_APPIMAGE", "unset", "ligus app (Linux): the desktop AppImage"),
            e("LIGUS_GITHUB_APP_CLIENT_ID", "built in", "ligus forge login: GitHub App client id"),
            e("LIGUS_GITLAB_CLIENT_ID", "built in", "ligus forge login: GitLab application id"),
        ],
    ),
];

/// Names read only inside the doctor's own probe child (an internal
/// protocol between `ligus doctor --turn-test` and itself), never by a
/// person.
#[cfg(test)]
pub const INTERNAL_ENV: &[&str] = &["LIGUS_PROBE_RAN", "LIGUS_PROBE_LEAK"];

/// The task guides.
pub struct Topic {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub text: &'static str,
}

pub const TOPICS: &[Topic] = &[
    Topic {
        name: "quickstart",
        aliases: &["start"],
        summary: "first device or another one: join or link this machine, then post a message",
        text: "\
Set up Ligus on this machine, then read and post. Every step ends with a command
that shows it worked.

1. Install the CLI (macOS, Linux):
     brew install langstroth-ai/ligus/ligus
     # or: curl -fsSL https://raw.githubusercontent.com/langstroth-ai/ligus/main/install.sh | sh
   verify: ligus --version
2. Is this your first device with Ligus?
   - I already use Ligus on another device: link this computer to that account
     instead (an invite would make a second you, with its own name and keys):
       ligus devices request --to ligusaddr1:…
       # the address: My devices > Add a computer on the other device; both
       # screens show the same code: compare it, then confirm there
     verify: ligus status
   - Yes, start here: get an invite from whoever runs the workspace (the desktop app:
     workspace menu > Invite) and save it to a file. It carries the workspace key:
     don't paste it on the command line.
       ligus enroll --file ./invite              # add --token @./relay-token for a gated relay
     verify: ligus status
3. Pull the workspace:
     ligus sync
   verify: ligus chats
4. Read and post:
     ligus tail <chat-id>
     ligus send <chat-id> \"hello\" --push
   verify: ligus tail <chat-id>

Next: an always-on box (ligus help box), a worker (ligus help worker), your
phone (ligus help phone). The desktop app: ligus app.
",
    },
    Topic {
        name: "setup",
        aliases: &["setup-flows", "wizard"],
        summary: "the guided setup flows: agent, tool, folder, box, phone, invite, relay",
        text: "\
Seven guided flows, the same in the desktop (⌘K > Set up an agent, Connect a tool, …;
or /setup in the composer), in lt and here. Each one checks first, writes only when
every check passed, can be run again safely (a finished step is skipped), and says
what to do when something fails (the `fix:` line under it).

  ligus setup                       what is done here, and what to run next
  ligus setup agent   --harness claude-code --name builder [--model sonnet] [--test-turn]
  ligus setup tool    --id notes --command \"npx -y @modelcontextprotocol/server-filesystem ~/notes\" [--reach builder]
  ligus setup tool    --id linear --kind url --url https://mcp.linear.app/mcp --token @./token
  ligus setup folder  --path ~/src/app
  ligus setup box     [--label home]          on the box; then --code - with the Admin's code
  ligus setup phone                           then: ligus pair, in your own terminal
  ligus setup invite                          invites are made on the desktop; this tracks them
  ligus setup relay   [--kind own --url https://relay.example.com] [--token - | @FILE]
  ligus setup runtimes [--apply]              the earlier `ligus setup`

For scripts and coding agents: every prompt is a flag, nothing asks. --json prints
one ligus.checks.v1 document (command \"setup <flow>\", plus flow, done, dryRun and
results). --dry-run checks and validates and writes nothing. Secrets (an MCP token, a
relay access token, an admission code) are read only from stdin (-) or an owner-only
file (@FILE), never from the command line, and are never printed.

Exit codes: 0 done; 1 a write or a check failed; 2 a missing or invalid flag; 3 a
tool, a sign-in or the agent sandbox is missing (the fix says how); 4 a person must
act (a passkey approval, pairing, creating an invite): stop and ask; 6 your role or
the workspace lockdown refuses it; 7 the relay or the server didn't answer.

A workspace with stricter device rules holds some writes (a workspace runtime, a
repository) for your passkey. The CLI has none: do that step in the desktop app, or
pass --recovery-words-file PATH (your recovery key's 24 words, an owner-only file).

Who may run what: adding an agent needs a Contributor (defining a workspace runtime
for it, an Admin); connecting a tool and binding a folder a Contributor; adding a
repository, admitting a box and inviting an Admin. A box is approved only with an
Admin's passkey, a phone is paired only in your own window or terminal, and no flag
can do either.
",
    },
    Topic {
        name: "box",
        aliases: &["serve-box", "home-host"],
        summary: "a headless serve box: install, admit, serve, pair (ligus-local, systemd, launchd)",
        text: "\
A `ligus serve` box is an always-on Ligus device (a home server, a spare Mac) that
syncs, runs the agents bound to it and serves the phone app. It joins a workspace
only through a box admission, as its own service member: never with a person's
invite (`ligus enroll` is for a person's own CLI device) and never as a person.
Use a data dir only this box uses, never the desktop app's.

What the code does: `ligus admit request` needs no relay, workspace or sync (it
creates the box identity and prints a request). `ligus admit redeem` keeps the
relay, room and workspace key the admission carries. `ligus serve` and `ligus
worker` sync first at start, then join with the kept admission; a box with no
copy of the workspace after that sync refuses to start. No `ligus sync` is needed.

With the deploy script (from a checkout of the repository; every step has --json):
  L=deploy/local/ligus-local
  1. $L preflight --role serve --data-dir ~/.ligus-serve
  2. $L install-cli                          # or: brew install langstroth-ai/ligus/ligus
  3. $L configure --role serve --data-dir ~/.ligus-serve --listen 127.0.0.1:8787 --label home-box
     (exits 4 until the box is admitted, and prints the next commands)
  4. Admit the box, with the env file loaded:
       set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a
     Live (code match), with the approval address an Admin gives you
     (ligus admit address, or the desktop's workspace menu > Admit a box):
       ligus admit request --to ligusaddr1:… --label home-box
     It prints Code: XXXX XXXX; the Admin's screen shows the same code, they confirm
     with a passkey, and the box keeps the admission once it is on the workspace log.
     Or paste:
       ligus admit request --label home-box   # prints ligusjoin1:… and a fingerprint
     HUMAN: an Admin approves it (desktop: workspace menu > Admit a box…, or the
     phone), checks the fingerprint, confirms with a passkey, sends back a code.
       ligus admit redeem ligusadmit1:…       # sealed to this box; or --file PATH
     verify: ligus status                     # admission: kept, workspace key: yes
  5. $L start --role serve --data-dir ~/.ligus-serve --systemd    # Linux user unit
     $L start --role serve --data-dir ~/.ligus-serve --launchd    # macOS LaunchAgent
  6. $L verify --role serve --data-dir ~/.ligus-serve --json
     verify: ligus status                     # member: yes
  `ligus-local help <command>` explains each step.

By hand:
  export LIGUS_DATA_DIR=~/.ligus-serve
  1. ligus --version                          # installed
  2. ligus doctor --require-confined          # exit 0: agents run sandboxed here
  3. ligus admit request --label home-box     # prints ligusjoin1:… and a fingerprint
  4. An Admin approves it: desktop, workspace menu > Admit a box… (or the phone),
     checks the fingerprint, approves with a passkey and sends back a code.
     (headless Admin, on their own CLI identity: ligus admit issue --request ligusjoin1:… \\
      --recovery-words-file PATH --out ./admission, then ligus sync)
       ligus admit redeem ligusadmit1:…       # or: ligus admit redeem --file ./admission
     verify: ligus status                     # admission: kept
  5. ligus serve --listen 127.0.0.1:8787 --label home-box   # syncs, then joins
     verify: curl -s http://127.0.0.1:8787/v1/health ; ligus status   # member: yes
  6. Pair the phone, from a second terminal with the same environment:
       ligus pair
     (ligus help phone; through a relay instead of the LAN or tailnet: ligus help remote)

A gated relay also needs a relay access token for the box: LIGUS_RELAY_ACCESS_TOKEN
(or LIGUS_RELAY_ACCESS_TOKEN_FILE) in its environment, or ligus auth login.
Keep it running: deploy/local/ligus-local start --systemd (Linux, a user unit) or
--launchd (macOS, a LaunchAgent that starts at login); `stop` with the same flag
removes it. Listen on loopback, a LAN or a Tailscale address; 0.0.0.0 is refused.
On Kubernetes the admission is a mounted Secret (LIGUS_ADMISSION_FILE): ligus help kubernetes.
",
    },
    Topic {
        name: "worker",
        aliases: &[],
        summary: "a worker host that answers mentions for the agents bound to it",
        text: "\
A worker answers @mentions for the agents bound to it, on workspace runtimes only
(never a personal key). Like a serve box it joins only through a box admission,
as its own member, never with a person's invite.

  export LIGUS_DATA_DIR=~/.ligus-worker
  1. ligus doctor --require-confined               # exit 0: agents run sandboxed here
  2. ligus admit request --label build-box          # no relay or sync needed
     HUMAN: an Admin approves it (desktop: workspace menu > Admit a box…)
     ligus admit redeem ligusadmit1:…               # or --file PATH
     verify: ligus status                           # admission: kept
  3. ligus setup runtimes                           # what this box can run (keys, Ollama, CLIs)
     export LIGUS_WS_SECRET_<ref>=…                 # the key of a secretRef runtime, on this box only
  4. ligus worker --label build-box --health-listen 127.0.0.1:8788   # syncs, then joins
     verify: curl -s 127.0.0.1:8788/v1/ready ; ligus status (member: yes) ; ligus runtimes
  5. A person defines runtimes and binds an agent to it (desktop, or their own CLI):
       ligus add-runtime anthropic Anthropic anthropic claude-sonnet-4-6 --secret-ref anthropic
       ligus add-agent coder anthropic --host <worker host id> && ligus sync
       # or: ligus set-agent-host coder <worker host id> && ligus sync
     verify: ligus agents ; ligus queue

As a service: deploy/local/ligus-local start --role worker --data-dir ~/.ligus-worker --systemd.
Claude Code on a worker is read-only unless LIGUS_WORKER_PERMISSION_MODE raises it;
write turns need the agent sandbox (ligus doctor).
",
    },
    Topic {
        name: "phone",
        aliases: &["pairing", "add-phone"],
        summary: "pair the phone with your box; Add a phone by QR, or paste its code",
        text: "\
The phone app talks to your `ligus serve` box. It needs two things: pairing (a link
from the box) and its own device key certified as one of your devices.

Pair:
  1. On the box, with the server's environment:  ligus pair
     It prints a QR code and a link (one-time, 10 minutes, five attempts). Through a
     relay (away from the LAN or tailnet): ligus help remote.
  2. On the phone: scan it in the Ligus app.
     verify: ligus phones list    (relay pairing) ; ligus remote status

Add the phone to your devices (scan the desktop's code):
  1. Desktop: Settings > Profile > My devices > Add a phone > Show code (5 minutes).
  2. Phone: Settings > This phone > Add to my devices, scan it, check whose code it
     is, choose Add. The phone shows its fingerprint. Its request reaches the desktop
     through your box (a phone not paired yet gets a provisional connection through
     the relay that reaches only the add-a-phone routes).
  3. Desktop: compare the fingerprint group by group, tick the box, Add this phone,
     confirm with your passkey. The phone pairs by itself.
     verify: the phone says \"This phone is now one of your devices\"

Can't scan? Paste the phone's code instead:
  1. Phone: Settings > This phone > Can't scan? Show this phone's code (ligusphone1:…).
  2. Desktop: Add a phone > Can't scan?, paste it, compare the fingerprint, confirm.
     Without a desktop, on a CLI identity of yours (never the box):
       ligus phones certify --code-file ./phone-code --recovery-words-file ~/.ligus-words \\
         --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX
       ligus sync
  3. Let the box sync a few seconds, then tap Retry on the phone (or ligus pair again).

Remove a phone: ligus phones revoke <pid|name> (also rotates serve.token unless --keep-token).
Errors: ligus help troubleshooting.
",
    },
    Topic {
        name: "remote",
        aliases: &["relay"],
        summary: "reach your box from the phone through a relay tunnel",
        text: "\
By default the phone reaches `ligus serve` only on the LAN or tailnet. A relay tunnel
lets it connect from anywhere; the relay sees only sealed traffic.

  1. ligus remote enable --relay https://relay.example.com [--reg-token @./reg-token]
     verify: ligus remote status                # relay: … (enabled), host id
  2. Restart serve (systemctl --user restart ligus-serve, or ligus-local stop/start).
     verify: ligus remote status                # serve: connected …
  3. ligus pair                                 # the link now includes the relay
     verify: ligus phones list
  4. ligus remote push-test                     # notifications, hop by hop

For one run only: ligus serve --remote-relay URL (or LIGUS_REMOTE_RELAY; off disables).
Stop: ligus remote disable (keeps keys and phones). New host id: ligus remote reset
(every phone pairs again). \"status pending\": the relay operator must allow the host
id. Rate limited (4029): ligus help troubleshooting.
",
    },
    Topic {
        name: "kubernetes",
        aliases: &["k8s", "helm"],
        summary: "one workspace on a cluster with deploy/kubernetes/ligus-k8s",
        text: "\
Kubernetes is a preview: build the image from deploy/container/Dockerfile and install
the deploy/helm/ligus chart from a checkout. deploy/kubernetes/ligus-k8s does it
non-interactively; always pass --context (never the current context), one namespace
per workspace, and --json for one JSON result on stdout.

  K=deploy/kubernetes/ligus-k8s
  1. $K preflight --context CTX -n NS --release R --probe --image REPO:TAG --json
  2. $K secrets workspace --throwaway --context CTX -n NS --release R      # trial only
     $K secrets workspace --from-dir DIR --context CTX -n NS --release R   # real workspace
  3. A real workspace: each box (serve, every worker) joins through its own box
     admission, never an invite. HUMAN: an Admin makes one per box on a desktop
     (workspace menu > Admit a box > For Kubernetes; headless: ligus admit issue
     --out) and saves it as a file named `admission` in a private DIR; then
       $K secrets generic --name R-serve-admission --from-dir DIR --context CTX -n NS --release R
     and a --values file with serve.admission.existingSecret: R-serve-admission
     (workers: worker.instances[].admission.existingSecret). The pod redeems it at
     start from LIGUS_ADMISSION_FILE.
  4. $K install --scenario trial --context CTX -n NS --release R --image REPO:TAG --dry-run --json
     $K install --scenario trial --context CTX -n NS --release R --image REPO:TAG --json
     (scenarios: trial, team-in-cluster-relay, team-external-relay; rerun to upgrade)
  5. $K verify --context CTX -n NS --release R --json
  6. $K teardown --context CTX -n NS --release R [--delete-data --delete-secrets --delete-namespace]

`ligus-k8s help <command>` explains each step; the ligus-deploy-k8s skill has the
admission details. The pods run `ligus serve` and `ligus worker` with
LIGUS_SECRET_BACKEND=k8s, LIGUS_ADMISSION_FILE and LIGUS_REQUIRE_SANDBOX=1 (exit 3 on
a node without Landlock or bwrap).
",
    },
    Topic {
        name: "repos",
        aliases: &["repositories"],
        summary: "workspace repositories, this host's checkouts, code hosts",
        text: "\
A workspace repository is synced; a checkout of it is per host. An agent turn for a
chat with a repository runs only in this host's confirmed checkout, on the chat's
branch.

  1. ligus repos add https://github.com/acme/web --branch main   # Admin, with presence
     verify: ligus repos list
  2. On each host that should work on it, either clone into the repos dir (the phone
     or desktop asks the host), or point at an existing checkout:
       ligus repos bind web ~/src/web
     or find them:
       ligus repos roots add ~/src && ligus repos scan && ligus repos confirm web ~/src/web
     verify: ligus repos list        # here: ~/src/web (…, confirmed)
  3. Give a chat its repository:  ligus repos chat <chat-id> web && ligus sync
  4. Code-host access for pushes and pull requests:
       ligus forge add github https://github.com            # uses the gh login
       ligus forge add gitlab https://gitlab.example.com
       printf %s \"$TOKEN\" | ligus forge token gitlab-gitlab.example.com write
     verify: ligus forge test <id>

LIGUS_REPOS_DIR (default <data dir>/repos) holds clones. SSH host aliases:
ligus repos alias; synced aliases: ligus repos aliases, then ligus repos accept.

No code yet? Start a scratch repository for a chat (an empty one, on this host):
  ligus repo scratch --chat <chat-id>       # removed after 14 quiet days unless kept
  ligus repo keep <scratch> --to ~/src/x    # keep it as an ordinary repository
",
    },
    Topic {
        name: "agents",
        aliases: &[],
        summary: "runtimes, agents, hosts and who may use them",
        text: "\
A runtime is a model or agent CLI (anthropic, openai, openrouter, ollama, custom,
claude-code, codex, antigravity, copilot). An agent is a name on a runtime, answered
by a host: its owner's device, or a worker or serve box.

  1. ligus setup runtimes                 # what this box can reach
     ligus setup runtimes --apply         # define the keyless ones
     ligus add-runtime <id> <name> <provider> <model> [--endpoint URL] [--secret-ref NAME]
     verify: ligus runtimes
  2. ligus add-agent coder <runtime-id> --host <host-id> --role \"writes code\"
     ligus sync
     verify: ligus agents ; ligus hosts
  3. In a chat: @coder … ; on the CLI: ligus send <chat-id> \"@coder …\" --push
     verify: ligus queue                  # empty once answered ; ligus tail <chat-id>

Credentials: --secret-ref NAME keeps the key off the shared log; every host running
the agent sets LIGUS_WS_SECRET_<NAME> (or a file in LIGUS_WS_SECRET_DIR). Keys are never stored
in the workspace (--secret was removed); ligus doctor warns about a key an older
version stored there (runtimes.inline-key): rotate it and use --secret-ref.

Local servers: a CLI that runs its own helper server (agy does) needs its agent's
local-servers switch, off by default and the Owner's:
  ligus agents set <agent> --local-servers on     # or: ligus setup agent … --local-servers
  verify: ligus agents ; ligus doctor --sandbox --agent <agent-id>
Linux with bubblewrap: each turn has its own loopback, where agy's server runs.
Linux Landlock tier: it stays off. macOS: not available yet (the Mac's sandbox
can't keep a listener loopback-only): run agy agents on a Linux host.

Access controls:
- Who may use an agent (only its owner, or everyone; daily turn limits; spending
  cap) is set on the agent in the desktop app (Workspace > Agent roster).
- LIGUS_MCP_MIN_ROLE: the author's minimum role for a turn at its full tier
  (default Admin for write-capable, Contributor for read-only); below it the turn
  runs tool-free or read-only.
- Workers run Claude Code read-only unless LIGUS_WORKER_PERMISSION_MODE is
  acceptEdits, runCommands or bypassPermissions; edits land in an isolated worktree
  as a proposal. Write tiers need the agent sandbox (ligus doctor).
- Repo-supplied runtimes and MCP servers stay inert until trusted: ligus trust list.
- ligus agent (development only) needs LIGUS_ALLOW_PERSONAL_AGENT and answers only
  its operator unless LIGUS_AGENT_OPEN_TO_MEMBERS=1.
",
    },
    Topic {
        name: "security",
        aliases: &["secrets", "data-dir"],
        summary: "the data dir, secret backends, LIGUS_DATA_DIR, LIGUS_SECRET_BACKEND",
        text: "\
The data dir is one Ligus device: its identity and keys, the decrypted event log
(ligus.db), config.toml (relay and room only), and for serve the phone credentials.
- LIGUS_DATA_DIR picks it (default ~/.ligus; ./ligus-data without HOME). It is
  created owner-only (0700). One process per data dir; never share one between
  machines, and never point a box at the desktop app's.
- Agents never read the data dir, the secret files or the secret store.

Secrets (the relay token, the workspace key, keys of runtimes) live in a secret
store, chosen by LIGUS_SECRET_BACKEND:
  keychain        macOS default: a Ligus keychain file in the data dir, unlocked by a
                  password in your login keychain. Each new build of the CLI asks once
                  to read it (a keychain prompt).
  secret-service  Linux desktop default (GNOME Keyring, KWallet)
  systemd-creds   Linux service hosts
  k8s             a mounted Kubernetes Secret (LIGUS_SECRETS_DIR)
  file            owner-only files in the data dir: the explicit opt-in for headless
                  boxes, scripts and tests (no prompts)

Secrets on the command line leak to ps and shell history: flags that take one
(--secret, --token, --reg-token) accept - (stdin) or @FILE (an owner-only file), and
*_FILE variables (LIGUS_WORKSPACE_KEY_FILE, LIGUS_RELAY_ACCESS_TOKEN_FILE,
LIGUS_REMOTE_REG_TOKEN_FILE, LIGUS_NTFY_TOKEN_FILE) read them from files.

For scripts and agents that run a ligus binary without touching your real
identity:  LIGUS_DATA_DIR=$(mktemp -d) LIGUS_SECRET_BACKEND=file ligus …
Hosts: LIGUS_REQUIRE_SANDBOX=1 refuses to start without an agent sandbox
(ligus doctor --require-confined is the same check).
",
    },
    Topic {
        name: "env",
        aliases: &["environment"],
        summary: "every LIGUS_* variable the CLI reads",
        text: "",
    },
    Topic {
        name: "troubleshooting",
        aliases: &["errors", "fix"],
        summary: "common errors and their fixes",
        text: "\
Start with:  ligus status ; ligus doctor ; ligus remote status

\"agy needs to run a local helper server\" / \"listen tcp 127.0.0.1:0: bind: operation not permitted\"
  The agent sandbox refused agy's helper server. On a Linux host with bubblewrap
  the workspace Owner turns on the agent's local-servers switch: ligus agents set
  <agent> --local-servers on (or Settings > Sandbox), then ligus sync. On the
  Landlock tier it stays off, and on macOS it isn't available yet (\"agy needs a
  local helper server, which the macOS sandbox can't allow safely yet\"): run agy
  agents on a Linux host with bubblewrap (ligus doctor --sandbox), or use another
  harness.

\"this box's workspace has no relay URL\" (sync, serve, worker, join)
  A serve or worker box: it has no admission yet (ligus admit request, an Admin
  approves, ligus admit redeem), or the admission's workspace has no relay.
  A person's CLI device: the invite came from a workspace with no relay.
  Either way: in the desktop set Settings > Team sync > Relay URL, then a new
  admission (box) or a new invite and `ligus enroll --file ./invite` (person);
  or set LIGUS_RELAY_URL.

\"this box isn't a member of workspace … and couldn't join\" / prints a join request
  The box has no admission: ligus admit request, an Admin approves (desktop:
  workspace menu > Admit a box…), ligus admit redeem, start it again.

\"this box has no copy of workspace … yet\" (serve, worker)
  The box syncs first at start and the relay gave it nothing: check the relay URL,
  the relay access token, LIGUS_WORKSPACE and that the admission is for this
  workspace. A box never founds a workspace. (register-worker: run ligus sync first.)

Relay 401 (unauthorized)
  The relay is gated: LIGUS_RELAY_ACCESS_TOKEN (or _FILE), or ligus auth login; a
  person's CLI can also pass it at enrollment: ligus enroll --file ./invite --token @./relay-token.

Phone: \"add this phone first\" (close 4040 device_not_certified)
  The box doesn't know the phone's key as yours: add the phone (ligus help phone),
  let the box sync, tap Retry; ligus pair again if the link used its five attempts.

Phone: \"Too many attempts\" (close 4029 rate_limited); box log: \"the relay is rate
limiting this network\"
  10 failed attempts a minute from one IP address (one /64) block it for 5 minutes;
  a phone on the box's network shares its address. Wait 5 minutes, check
  ligus remote status, then ligus pair once.

Keychain password prompts on macOS with a development build
  The keychain trusts a build by its code hash, so each rebuild asks again. Sign
  the build, or for tests and scripts use a throwaway data dir:
  LIGUS_DATA_DIR=$(mktemp -d) LIGUS_SECRET_BACKEND=file ligus …

macOS keeps asking about ai.ligus.secrets.unlock, or \"didn't let Ligus read its
keychain unlock password\"
  The keychain items were saved by a different Ligus build. Enter your login
  password and choose Always Allow; a signed release of the app then re-creates the
  items so they trust it (the new item is written before the old one goes) and
  nothing asks again. The CLI and development builds don't repair items: run the
  build that saved them. If macOS asks for the \"ligus\" keychain password, choose
  Cancel: it is random and only Ligus knows it. Over SSH macOS can't ask: run the
  same command with the same data dir once in a desktop terminal. ligus doctor
  reports secrets.keychain-access without prompting. Don't delete the keychain or
  its unlock item: this device's keys are in it.

Ubuntu: \"agent sandbox: landlock\" instead of bwrap, or bwrap fails to create a
user namespace
  Ubuntu 24.04 restricts unprivileged user namespaces (AppArmor,
  kernel.apparmor_restrict_unprivileged_userns=1) and ships no profile for bwrap, so
  Ligus falls back to the Landlock tier (Linux 6.7+). For bwrap install the profile:
    sudo install -m 0644 deploy/linux/apparmor/ligus-bwrap /etc/apparmor.d/ligus-bwrap
    sudo apparmor_parser -r /etc/apparmor.d/ligus-bwrap
  verify: ligus doctor --expect-tier bwrap. No sandbox at all: install bubblewrap.

serve won't start: \"refusing to listen on 0.0.0.0\"
  Use a loopback, LAN or Tailscale address (or LIGUS_SERVE_ALLOW_ANY_BIND=1 behind
  something else that controls access).

serve or worker exits 3
  LIGUS_REQUIRE_SANDBOX=1 and no agent sandbox: ligus doctor says why.

Every command exits 2 at start with a model gateway error
  The file LIGUS_MODEL_GATEWAYS_FILE names can't be used: fix it or unset the
  variable. ligus gateway-check tests the gateways.

A worker doesn't answer: ligus agents (bound to this host?), ligus queue (host
online?), ligus runtimes (secretRef ✓?), and its log for \"sync:\" lines.
",
    },
    Topic {
        name: "exit-codes",
        aliases: &["exit", "exit-code", "codes"],
        summary: "the exit codes every Ligus tool shares, and the error shape",
        text: "\
One table for every Ligus tool: ligus, langstroth-relay and the deploy scripts
(deploy/local/ligus-local, deploy/kubernetes/ligus-k8s).

  code  meaning                                                   what to do
  0     done (warnings allowed)                                   continue
  1     failed: the message (or checks[].fix) says why            read it, fix, retry
  2     usage error: unknown command, missing or bad argument     fix the command line
  3     a precondition you can fix is missing (a tool, a file,    fix it and rerun
        a sign-in, the agent sandbox for serve and worker)
  4     a person must decide or act (a passkey, an admission,     stop and ask
        exposure, deleting owned data)
  5     not found (a workspace, chat, proposal, run)              check the id
  6     refused by workspace rules (a role, lockdown, agent       ask an Admin; don't retry
        access)
  7     offline, the relay unreachable, or timed out              retry with backoff
  8+    command-specific, listed in that command's help

ligus uses: 1 for a failed check or operation; 2 for usage (and a model gateways
file that can't be used); 3 when serve or worker refuses to start without an
agent sandbox (LIGUS_REQUIRE_SANDBOX=1) or the relay rejected the access token;
5 for an unknown chat; 6 when the workspace refuses (a role, an agent's access);
7 when the relay can't be reached or asks to wait. ligus doctor and ligus
gateway-check exit 1 when a check fails; before 0.2 they exited 3, 4, 5
(doctor's preflights) and 6 (gateway-check), and --legacy-exit-codes keeps
those codes for one release.

The deploy scripts use 0 to 4 (an unreachable cluster is 3); langstroth-relay
uses 0 to 2.

Errors print the same way in every tool: \"ligus: <what>\" on stderr, then
indented \"why:\" and \"fix:\" lines when known (a usage error's fix names the
help to read). With --json (or -o json) the error is also the one document on
stdout: {\"schema\": \"ligus.error.v1\", \"tool\", \"command\", \"exitCode\",
\"error\": {\"code\", \"message\", \"why\", \"fix\"}}; code is usage, not_found,
refused, relay_unreachable, relay_busy, relay_token_rejected, unreachable or
error.
",
    },
    Topic {
        name: "checks",
        aliases: &["checks.v1", "ligus.checks.v1", "doctor-json"],
        summary: "the ligus.checks.v1 report every doctor and check command prints",
        text: "\
One report shape for every check command: ligus doctor --json, langstroth-relay
check --json, and the deploy scripts (deploy/local/ligus-local and
deploy/kubernetes/ligus-k8s, whose ligus.agent-deploy.v1 is a conforming
profile with the same fields). One JSON document on stdout:

  {\"schema\": \"ligus.checks.v1\", \"tool\": \"ligus\", \"version\": \"0.1.1\",
   \"command\": \"doctor\", \"ok\": false, \"exitCode\": 1,
   \"checks\": [{\"id\": \"relay.reachable\", \"title\": \"relay\", \"status\": \"fail\",
               \"detail\": \"https://relay.example.com: connection refused\",
               \"fix\": \"check the URL and this network; on the relay host: ...\"}],
   \"next\": [\"langstroth-relay check --url https://relay.example.com\"]}

  schema     always ligus.checks.v1 (a profile keeps its own name)
  tool       the program; command: its subcommand; version: its version
  ok         true when no check failed (warnings allowed)
  exitCode   the process exit code (ligus help exit-codes)
  checks[]   id (stable, dotted), title (for people), status (ok, warn, fail
             or skip), detail (one line), fix (one line, when not ok), notes
             (optional, more lines of what was found)
  next[]     commands to run next, in order; empty when there is nothing to do

Consumers ignore fields they don't know; new fields never rename old ones. In
the deploy scripts' profile a passed check says pass (read it as ok) and info
is a finding that is neither good nor bad.

The text form: one line per check (status, title, detail), an indented fix:
line, then a summary such as \"1 warning · next: ligus sync\". ligus doctor
keeps its first two lines as \"agent sandbox: <tier>\" and the tier's summary,
and the turn test's findings as \"turn test: ...\" lines (scripts read them).

Each doctor names the next one down: ligus doctor's relay check says to run
langstroth-relay check --url <relay> on the relay host.
",
    },
    Topic {
        name: "agents-guide",
        aliases: &["AGENTS", "agents-playbook", "playbook", "agent-guide"],
        summary: "a one-shot playbook for coding agents: set up, add an agent, k8s, health",
        text: "\
For coding agents operating Ligus. Every ligus command is non-interactive when
stdin isn't a terminal (no prompts; pass the flag instead), prints errors on stderr
as \"ligus: …\", and exits with one table every Ligus tool shares (ligus help
exit-codes): 0 done, 1 failed, 2 usage, 3 a precondition you can fix, 4 a person
must decide (stop and ask), 5 not found, 6 refused by workspace rules, 7 offline. Read commands take --json. Never run a ligus binary against
the person's real data dir to experiment: use
  export LIGUS_DATA_DIR=$(mktemp -d) LIGUS_SECRET_BACKEND=file
Human-only steps: approving a box admission with a passkey, enrolling a person's
own CLI device with a real invite (it carries the workspace key; never for a box),
pairing the phone, exposing a port. A box (serve, worker) never runs ligus enroll.

Set up a box (serve) on this machine:
  L=deploy/local/ligus-local
  $L preflight --role serve --data-dir ~/.ligus-serve --json         # verify: ok true
  $L install-cli --json                                              # verify: ligus --version
  $L configure --role serve --data-dir ~/.ligus-serve --json         # exit 4 until admitted; prints next[]
  (env file loaded) ligus admit request --to ligusaddr1:… --label <name> --json
      # the approval address comes from an Admin (ligus admit address, or the desktop);
      # it waits for an approver, prints Code: XXXX XXXX on stderr (check id match-code),
      # and keeps the admission only once it is on the workspace log. Exit 3 (no
      # mailbox on the relay): use the paste path below. Exit 4: nobody approved in --wait.
  HUMAN: an Admin approves it in their Inbox or on the phone, checking the same code
  (paste path: ligus admit request --label <name>, HUMAN approves it on the desktop
   and sends a code, then ligus admit redeem ligusadmit1:…)
  verify: ligus status --json (admissionKept)
  $L start --role serve --data-dir ~/.ligus-serve --systemd|--launchd --json
  $L verify --role serve --data-dir ~/.ligus-serve --json            # verify: ok true
  LIGUS_DATA_DIR=~/.ligus-serve ligus status --json                  # verify: member true

Guided setup (each flow checks first, is safe to rerun, exits 4 at a human-only step):
  ligus setup --json                                                 # what is done, what to run next
  ligus setup agent --harness claude-code --name <name> --json       # verify: done true

Add an agent (from a person's CLI device or the desktop, not the box):
  ligus setup runtimes --apply                                       # verify: ligus runtimes --json
  ligus add-agent <name> <runtime-id> --host <host-id> && ligus sync # verify: ligus agents --json
  ligus send <chat-id> \"@<name> ping\" --push                        # verify: ligus queue (empty), ligus tail <chat-id>

Deploy on Kubernetes:
  K=deploy/kubernetes/ligus-k8s   (always --context; confirm the cluster with the human)
  $K preflight --context CTX -n NS --release R --json
  $K secrets workspace --throwaway|--from-dir DIR --context CTX -n NS --release R --json
  $K install --scenario trial --context CTX -n NS --release R --image REPO:TAG --json
  $K verify --context CTX -n NS --release R --json                   # verify: ok true

Check health:
  ligus status --json                 # relay, key, local copy, member, serve, sandbox tier
  ligus doctor --require-confined     # exit 1: no agent sandbox (sandbox.tier fails)
  ligus remote status --json          # serve's relay link and phones
  curl -s http://<listen>/v1/health   # serve ; worker: --health-listen /v1/ready
  ligus hosts --json ; ligus queue

Find more: ligus help (commands and topics), ligus <command> --help,
ligus help env, ligus help troubleshooting, AGENTS.md in the repository.
",
    },
];

fn find_cmd(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}

fn find_topic(name: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.name == name || t.aliases.iter().any(|a| a.eq_ignore_ascii_case(name)))
}

/// `ligus help` with no argument, in the shared section order.
pub fn overview() -> String {
    let mut s = format!(
        "NAME\n  ligus {} - the headless Ligus client (Ligus is short for ligustica)\n\n\
         USAGE\n  ligus <command> [<subcommand>] [flags]\n  ligus help <command> [<subcommand>] | <topic>\n  \
         ligus help --json\n  ligus <command> --help\n\nCOMMANDS\n",
        env!("CARGO_PKG_VERSION")
    );
    for (i, g) in GROUPS.iter().enumerate() {
        let _ = write!(s, "{}  {}\n", if i == 0 { "" } else { "\n" }, g);
        for c in COMMANDS.iter().filter(|c| c.group == *g && !c.name.contains(' ')) {
            let _ = writeln!(s, "    {:<16} {}", c.name, c.summary);
        }
    }
    s.push_str("\nTOPICS (ligus help <topic>)\n");
    for t in TOPICS {
        let _ = writeln!(s, "  {:<16} {}", t.name, t.summary);
    }
    s.push_str(
        "\nFLAGS\n  Flags go anywhere after the command; a value is the next argument or follows an equals sign\n  \
         (--label box, or --label=box). Every command also takes -o json|text (-o json is --json; jsonl for\n  \
         streams), -q (only what needs attention) and --color auto|always|never (colour only on a terminal;\n  \
         NO_COLOR and CLICOLOR_FORCE count; JSON is never coloured). ligus never asks for confirmation, so\n  \
         there is no --yes; a passkey step is always a person's.\n\n\
         EXAMPLES\n  ligus status\n  ligus doctor --json\n  ligus help agents-guide\n\n\
         ENVIRONMENT\n  Every command works on one data dir: LIGUS_DATA_DIR (default ~/.ligus). Env vars override the\n  \
         connection `ligus enroll` or `ligus admit redeem` saved. ligus help env lists every variable.\n\n\
         EXIT CODES (one table for every Ligus tool: ligus help exit-codes)\n  \
         0 done   1 failed (\"ligus: …\" on stderr)   2 usage error   3 a precondition you can fix   \
         4 a person must decide\n  5 not found   6 refused by workspace rules   7 offline or timed out\n\n\
         SEE ALSO\n  ligus help agents-guide (an agent setting Ligus up), AGENTS.md in the repository\n  \
         deploy/local/ligus-local --help and deploy/kubernetes/ligus-k8s --help (deploying serve and workers)\n  \
         langstroth-relay help (the relay), the desktop app (ligus app)\n",
    );
    s
}

fn env_lookup(name: &str) -> Option<&'static EnvVar> {
    ENV.iter().flat_map(|(_, vs)| vs.iter()).find(|v| v.name == name)
}

/// Variables a command reads, common ones included.
fn env_of(c: &Cmd) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    if c.data {
        v.extend(["LIGUS_DATA_DIR", "LIGUS_SECRET_BACKEND"]);
    }
    if c.conn {
        v.extend(["LIGUS_RELAY_URL", "LIGUS_RELAY_ACCESS_TOKEN", "LIGUS_WORKSPACE", "LIGUS_WORKSPACE_KEY"]);
    }
    for n in c.env {
        if !v.contains(n) {
            v.push(n);
        }
    }
    v
}

/// One command's reference, in the section order every Ligus tool uses:
/// NAME, USAGE, COMMANDS, FLAGS, EXAMPLES, ENVIRONMENT, EXIT CODES, SEE ALSO.
pub fn command_help(c: &Cmd) -> String {
    let mut s = format!("NAME\n  ligus {} - {}\n\n", c.name, c.summary);
    for line in wrap(c.purpose, 86) {
        let _ = writeln!(s, "  {line}");
    }
    s.push_str("\nUSAGE\n");
    for l in c.synopsis {
        let _ = writeln!(s, "  {l}");
    }
    let subs: Vec<&Cmd> = COMMANDS
        .iter()
        .filter(|x| x.name.strip_prefix(c.name).is_some_and(|r| r.starts_with(' ') && !r[1..].contains(' ')))
        .collect();
    if !subs.is_empty() {
        s.push_str("\nCOMMANDS\n");
        for x in subs {
            let _ = writeln!(s, "  {:<16} {}", &x.name[c.name.len() + 1..], x.summary);
        }
    }
    s.push_str("\nFLAGS\n");
    let common = [f("-h, --help", "", "print this help"), f("-o, -q, --color", "", "every command: -o json|text, quiet, colour (ligus help)")];
    for fl in c.flags.iter().chain(common.iter()) {
        let def = if fl.default.is_empty() { String::new() } else { format!(" (default: {})", fl.default) };
        let _ = writeln!(s, "  {:<28} {}{def}", fl.flag, fl.about);
    }
    s.push_str("\nEXAMPLES\n");
    for x in c.examples {
        let _ = writeln!(s, "  {x}");
    }
    s.push_str("\nENVIRONMENT\n");
    let env = env_of(c);
    let width = env.iter().map(|n| n.len()).max().unwrap_or(0).max(28);
    if env.is_empty() {
        s.push_str("  (none)\n");
    }
    for n in env {
        let about = env_lookup(n).map(|v| v.about).unwrap_or(match n {
            "HOSTNAME" => "the host name, for default labels",
            "PATH" => "where installed programs are found",
            "ANTHROPIC_API_KEY" | "OPENAI_API_KEY" | "OPENROUTER_API_KEY" => "a provider's API key",
            "DISPLAY" | "WAYLAND_DISPLAY" => "the graphical session (Linux)",
            _ => "",
        });
        let _ = writeln!(s, "  {n:<width$} {about}");
    }
    s.push_str("\nEXIT CODES\n  0  success\n  1  error (the message is on stderr)\n  2  usage error: missing or bad argument, unknown subcommand\n");
    for (code, why) in c.exit {
        if *code > EXIT_USAGE {
            let _ = writeln!(s, "  {code}  {why}");
        }
    }
    s.push_str(
        "  3 a precondition you can fix, 4 a person must decide, 5 not found, 6 refused by workspace rules,\n  \
         7 offline or timed out: the table every Ligus tool shares (ligus help exit-codes)\n",
    );
    s.push_str("\nSEE ALSO\n");
    let _ = writeln!(s, "  {}", c.see_also.join(", "));
    s
}

/// `ligus help --json`: every command and topic as one `ligus.help.v1`
/// document, for agents and completion scripts.
pub fn help_json() -> serde_json::Value {
    use serde_json::json;
    json!({
        "schema": "ligus.help.v1",
        "tool": "ligus",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": COMMANDS.iter().map(|c| json!({
            "name": c.name,
            "group": c.group,
            "summary": c.summary,
            "description": c.purpose.split_whitespace().collect::<Vec<_>>().join(" "),
            "usage": c.synopsis,
            "flags": c.flags.iter().map(|fl| json!({ "flag": fl.flag, "default": fl.default, "about": fl.about })).collect::<Vec<_>>(),
            "environment": env_of(c),
            "examples": c.examples,
            "exitCodes": c.exit.iter().map(|(code, why)| json!({ "code": code, "meaning": why })).collect::<Vec<_>>(),
            "seeAlso": c.see_also,
        })).collect::<Vec<_>>(),
        "topics": TOPICS.iter().map(|t| json!({ "name": t.name, "aliases": t.aliases, "summary": t.summary })).collect::<Vec<_>>(),
        "exitCodes": crate::problem::TABLE.iter().map(|(code, meaning, todo)| json!({ "code": code, "meaning": meaning, "do": todo })).collect::<Vec<_>>(),
    })
}

/// `ligus help env`.
pub fn env_help() -> String {
    let mut s = String::from(
        "Every LIGUS_* variable the ligus CLI reads. A blank value is unset; booleans take 1/true/on/yes\n\
         and 0/false/off/no. Env wins over config.toml (ligus enroll, ligus admit redeem). The *_FILE variables read the\n\
         secret from a file instead (not both). Other names: LIGUS_MCP_MAX_* turn budgets,\n\
         LIGUS_ACP_*, LIGUS_AGENT_MAX_*, LIGUS_OLLAMA_WIRE, LIGUS_PATCH_LEAVING_CALLS (see the docs:\n\
         Environment variables).\n",
    );
    for (section, vars) in ENV {
        let _ = write!(s, "\n{}\n", section.to_uppercase());
        for v in *vars {
            let _ = writeln!(s, "  {:<40} {}  [default: {}]", v.name, v.about, v.default);
        }
    }
    s
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for w in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + w.len() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The command `words` name: `<command> <subcommand>` when that exists,
/// else `<command>`.
fn cmd_of(words: &[&str]) -> Option<&'static Cmd> {
    let first = match *words.first()? {
        "--version" | "-V" => "version",
        w => w,
    };
    words.get(1).and_then(|w| find_cmd(&format!("{first} {w}"))).or_else(|| find_cmd(first))
}

/// The words of a command line, without flags (`--help`, `-h`, `--json`, `-`).
fn words<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().filter(|a| !a.starts_with('-')).collect()
}

/// `ligus <args> --help`: that command's reference (the overview with no
/// command). Topics are only for `ligus help <topic>`.
pub fn lookup_flag(args: &[&str]) -> Option<String> {
    let w = words(args);
    if w.is_empty() {
        return Some(overview());
    }
    cmd_of(&w).map(command_help)
}

/// `ligus help <args>`: a command's reference, a topic, or both when a name
/// is both (the guide first).
pub fn lookup(args: &[&str]) -> Option<String> {
    let words = words(args);
    if words.is_empty() {
        return Some(overview());
    }
    let topic = (words.len() == 1).then(|| find_topic(words[0])).flatten();
    let cmd = cmd_of(&words);
    match (topic, cmd) {
        (Some(t), Some(c)) => Some(format!("{}\n{}\n{}", topic_text(t), "─".repeat(60), command_help(c))),
        (Some(t), None) => Some(topic_text(t)),
        (None, Some(c)) => Some(command_help(c)),
        (None, None) => None,
    }
}

fn topic_text(t: &Topic) -> String {
    if t.name == "env" {
        return env_help();
    }
    format!("ligus help {}: {}\n\n{}", t.name, t.summary, t.text)
}

/// `ligus help …` (`topics`) / `ligus … --help`: print and exit 0, or name
/// the unknown one and exit 2.
pub fn print_and_exit(args: &[&str], topics: bool) -> ! {
    if topics && args == ["--json"] {
        println!("{}", help_json());
        std::process::exit(EXIT_OK);
    }
    match if topics { lookup(args) } else { lookup_flag(args) } {
        Some(text) => {
            print!("{text}");
            std::process::exit(EXIT_OK);
        }
        None => {
            eprintln!(
                "ligus: no command or topic {:?}. `ligus help` lists the commands and topics.",
                args.iter().filter(|a| !a.starts_with('-')).copied().collect::<Vec<_>>().join(" ")
            );
            std::process::exit(EXIT_USAGE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn src(rel: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel)).unwrap()
    }

    /// The text between `start` and the next line equal to `end` after it.
    fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
        let i = text.find(start).unwrap_or_else(|| panic!("{start:?} not found"));
        let rest = &text[i..];
        let j = rest.find(end).unwrap_or(rest.len());
        &rest[..j]
    }

    /// `"name"` arms (`"x" =>`, `"x" if … =>`) at the start of a line.
    fn top_arms(block: &str) -> BTreeSet<String> {
        block
            .lines()
            .filter_map(|l| {
                let t = l.trim_start();
                let rest = t.strip_prefix('"')?;
                let (name, after) = rest.split_once('"')?;
                (after.trim_start().starts_with("=>") || after.trim_start().starts_with("if ")).then(|| name.to_string())
            })
            .collect()
    }

    /// `Some("x")` and `Some(v @ ("x" | "y"))` literals in a block.
    fn some_arms(block: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for part in block.split("Some(").skip(1) {
            let part = part.trim_start();
            if let Some(r) = part.strip_prefix('"') {
                if let Some((n, _)) = r.split_once('"') {
                    out.insert(n.to_string());
                }
            } else if let Some((_, alts)) = part.split_once("@ (") {
                let alts = alts.split(')').next().unwrap_or("");
                for a in alts.split('|') {
                    out.insert(a.trim().trim_matches('"').to_string());
                }
            }
        }
        out
    }

    fn sections_ok(name: &str) {
        let c = find_cmd(name).unwrap_or_else(|| panic!("no help entry for `ligus {name}`"));
        let h = command_help(c);
        // One section order for every Ligus tool.
        let mut at = 0;
        for sec in ["NAME", "USAGE", "FLAGS", "EXAMPLES", "ENVIRONMENT", "EXIT CODES", "SEE ALSO"] {
            let i = if sec == "NAME" { h.find("NAME\n") } else { h.find(&format!("\n{sec}\n")) };
            let i = i.unwrap_or_else(|| panic!("`ligus {name} --help` lacks {sec}:\n{h}"));
            assert!(i >= at, "`ligus {name} --help`: {sec} is out of order:\n{h}");
            at = i;
        }
        assert!(!c.purpose.is_empty() && !c.synopsis.is_empty(), "{name}: empty purpose or synopsis");
        assert!((1..=3).contains(&c.examples.len()), "{name}: 1-3 examples, has {}", c.examples.len());
        assert!(!c.see_also.is_empty(), "{name}: no see also");
        assert!(c.examples.iter().all(|x| x.contains("ligus")), "{name}: examples must be ligus command lines");
        for fl in c.flags {
            assert!(!fl.about.is_empty(), "{name} {}: no description", fl.flag);
        }
        // `ligus help <name>` and `ligus <name> --help` both reach it.
        let words: Vec<&str> = name.split(' ').collect();
        assert!(lookup(&words).unwrap().contains(&h), "ligus help {name}");
        let mut with_flag = words.clone();
        with_flag.push("--help");
        assert_eq!(lookup_flag(&with_flag).as_deref(), Some(h.as_str()), "ligus {name} --help");
    }

    /// Every command the dispatcher handles has a complete help entry, and
    /// every help entry is a real command.
    #[test]
    fn every_dispatched_command_has_complete_help() {
        let main = src("main.rs");
        let run = between(&main, "    match cmd {\n", "\n    }\n}\n");
        let mut want: BTreeSet<String> = top_arms(run);
        assert!(want.len() >= 35, "dispatcher parse found only {want:?}");
        want.insert("help".into());
        want.insert("version".into());
        want.insert("app".into());
        want.insert("hook".into());
        for name in &want {
            sections_ok(name);
        }
        // Subcommands, from each subcommand dispatcher. Each entry: the
        // command, its source and the block that dispatches it, and the
        // `Some("…")` literals in it that aren't subcommands.
        let groups: &[(&str, String, &str, &str, &[&str])] = &[
            ("auth", src("auth.rs"), "pub fn cmd_auth", "\n}\n", &[]),
            ("admit", src("admit.rs"), "pub fn cmd_admit", "\n}\n", &[]),
            ("admit", main.clone(), "\"admit\" => {", "\n        \"approvals\"", &[]),
            ("approvals", main.clone(), "\"approvals\" => match", "\n        },\n", &[]),
            ("recovery-key", src("admit.rs"), "pub fn cmd_recovery_key", "\n}\n", &[]),
            ("config", main.clone(), "\"config\" => match", "\n        },\n", &[]),
            ("remote", main.clone(), "\"remote\" => {", "\n        }\n", &[]),
            ("phones", main.clone(), "\"phones\" => {", "\n        \"dashboard\"", &[]),
            ("devices", main.clone(), "\"devices\" => {", "\n        \"recovery-key\"", &[]),
            ("profile", main.clone(), "\"profile\" => match", "\n        },\n", &[]),
            ("agents", main.clone(), "\"agents\" => match", "\n        },\n", &[]),
            ("dashboard", main.clone(), "\"dashboard\" => match", "\n        },\n", &[]),
            ("facts", main.clone(), "\"facts\" => {", "\n        }\n", &[]),
            ("trust", main.clone(), "fn cmd_trust", "\n}\n", &[]),
            ("memory", src("memory_cmd.rs"), "pub fn run(", "\n}\n", &[]),
            ("asks", src("asks_cmd.rs"), "pub fn run(", "\n}\n", &[]),
            ("hook", src("hook_cmd.rs"), "pub fn cmd(", "\n}\n", &[]),
            ("repos", src("repos_cmd.rs"), "pub fn cmd(", "\n}\n", &["on", "off", "default"]),
            ("repo", src("scratch_cmd.rs"), "pub fn cmd_repo(", "\n}\n", &[]),
            ("forge", src("forge_cmd.rs"), "pub fn cmd(", "\n}\n", &["oauth", "gh", "none"]),
            ("setup", src("setup_cmd.rs"), "pub fn cmd(", "\n}\n", &["-", "--json", "--dry-run"]),
        ];
        let mut seen_subs = BTreeSet::new();
        for (cmd, text, start, end, not_subs) in groups {
            let block = between(text, start, end);
            let subs: BTreeSet<String> = some_arms(block).into_iter().filter(|s| !not_subs.contains(&s.as_str())).collect();
            assert!(!subs.is_empty(), "no subcommands parsed for {cmd}");
            for s in subs {
                // `repos roots add` and `forge add --auth token` reuse names
                // a subcommand has: they're covered by that subcommand.
                let name = format!("{cmd} {s}");
                sections_ok(&name);
                seen_subs.insert(name);
            }
            sections_ok(cmd);
        }
        // No help entry for something that doesn't exist.
        for c in COMMANDS {
            let top = c.name.split(' ').next().unwrap();
            assert!(want.contains(top), "help entry `{}` is not a command", c.name);
            if c.name.contains(' ') {
                assert!(seen_subs.contains(c.name) || c.name == "forge git-credential", "help entry `{}` is not a dispatched subcommand", c.name);
            }
        }
    }

    /// Every flag the source reads (`flag_val("--x")`, `has("--x")`,
    /// `flag("--x")`) is documented on some command.
    #[test]
    fn every_flag_read_is_documented() {
        let documented: BTreeSet<String> = COMMANDS
            .iter()
            .flat_map(|c| c.flags.iter())
            .flat_map(|fl| fl.flag.split([' ', ',']).filter(|w| w.starts_with("--")).map(|w| w.to_string()).collect::<Vec<_>>())
            .collect();
        let mut missing = Vec::new();
        for file in ["main.rs", "admit.rs", "repos_cmd.rs", "scratch_cmd.rs", "hook_cmd.rs"] {
            let text = src(file);
            for pat in ["flag_val(\"", "has(\"", "flag(\""] {
                for part in text.split(pat).skip(1) {
                    let name = part.split('"').next().unwrap();
                    if name.starts_with("--") && !documented.contains(name) && name != "--help" {
                        missing.push(format!("{file}: {name}"));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "flags read but in no command's help: {missing:?}");
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// Every `LIGUS_*` variable the CLI's own (non-test) code names is in
    /// `ligus help env`, and every variable a command lists is described.
    #[test]
    fn every_env_var_read_is_in_help_env() {
        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let env = env_help();
        let mut missing = BTreeSet::new();
        for p in files {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if name == "tests.rs" || name.ends_with("_tests.rs") || name == "testkit.rs" || name == "help.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            // The unit tests at the end of a file aren't what the CLI reads.
            let code = text.split("#[cfg(test)]\nmod tests").next().unwrap();
            for part in code.split("\"LIGUS_").skip(1) {
                let var: String = part.chars().take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_').collect();
                let var = format!("LIGUS_{var}");
                if part[var.len() - 6..].starts_with('"') && !INTERNAL_ENV.contains(&var.as_str()) && !env.contains(&format!("  {var} ")) {
                    missing.insert(format!("{var} ({name})"));
                }
            }
        }
        for name in crate::hardening::FILE_SECRETS {
            for v in [name.to_string(), format!("{name}_FILE")] {
                if !env.contains(&format!("  {v} ")) {
                    missing.insert(v);
                }
            }
        }
        assert!(missing.is_empty(), "LIGUS_* variables read but not in `ligus help env`: {missing:?}");
        for c in COMMANDS {
            for n in env_of(c) {
                if n.starts_with("LIGUS_") {
                    assert!(env_lookup(n).is_some(), "{}: {n} is not in ENV", c.name);
                }
            }
        }
    }

    /// The docs' CLI reference lists every command and mentions the topics.
    #[test]
    fn docs_cli_reference_lists_every_command() {
        let md = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs-site/docs/ligus/reference/cli.md")).unwrap();
        let mut missing = Vec::new();
        for c in COMMANDS {
            let shown = match c.name {
                "version" => "ligus --version".to_string(),
                n => format!("ligus {n}"),
            };
            if !md.contains(&shown) {
                missing.push(shown);
            }
        }
        for t in TOPICS {
            if !md.contains(&format!("`{}`", t.name)) {
                missing.push(format!("topic {}", t.name));
            }
        }
        assert!(missing.is_empty(), "docs-site/docs/ligus/reference/cli.md is missing: {missing:?}");
    }

    #[test]
    fn topics_and_lookup() {
        for t in TOPICS {
            let h = lookup(&[t.name]).unwrap();
            assert!(h.len() > 200, "topic {} is empty", t.name);
            for a in t.aliases {
                assert!(lookup(&[a]).unwrap().starts_with(&topic_text(t)), "alias {a}");
            }
        }
        // A topic that is also a command prints both.
        let both = lookup(&["agents"]).unwrap();
        assert!(both.contains("Access controls") && both.contains("ligus agents [--json]"), "{both}");
        // Unknown.
        assert_eq!(lookup(&["no-such-thing"]), None);
        assert!(lookup(&[]).unwrap().contains("TOPICS"));
        // The overview lists every top-level command and topic.
        let o = overview();
        for c in COMMANDS.iter().filter(|c| !c.name.contains(' ')) {
            assert!(o.contains(&format!("  {} ", c.name)), "overview lacks {}", c.name);
        }
        // The overview says both flag spellings work and names the one
        // error prefix; no help text names another prefix.
        assert!(o.contains("--label box, or --label=box") && o.contains("\"ligus: …\" on stderr"), "{o}");
        for t in TOPICS {
            assert!(!t.text.contains("\"error: "), "topic {} names the old error prefix", t.name);
        }
        // `ligus remote enable --relay x --help`: the subcommand's help.
        assert!(lookup_flag(&["remote", "enable", "--relay", "https://r.example", "--help"]).unwrap().starts_with("NAME\n  ligus remote enable"));
        // `ligus agents --help` is the command only; `ligus box --help` is no command.
        assert!(!lookup_flag(&["agents", "--help"]).unwrap().contains("Access controls"));
        assert_eq!(lookup_flag(&["box", "--help"]), None);
        // A guide ends each step with a verify command.
        for name in ["quickstart", "box", "worker", "agents-guide"] {
            assert!(lookup(&[name]).unwrap().contains("verify:"), "{name}");
        }
        // No business intent, no old name.
        for t in TOPICS {
            for bad in ["pricing", "Hive", "hosted plan", "swarmwatch"] {
                assert!(!t.text.contains(bad), "topic {} says {bad}", t.name);
            }
        }
    }

    /// The quickstart asks the first run's question before anything joins:
    /// someone who already uses Ligus links this computer, and only a first
    /// device enrolls with an invite.
    #[test]
    fn quickstart_asks_first_device_before_joining() {
        let t = find_topic("quickstart").unwrap().text;
        let ask = t.find("Is this your first device with Ligus?").expect("the question");
        let link = t.find("ligus devices request").expect("the link path");
        let enroll = t.find("ligus enroll").expect("the invite path");
        assert!(ask < link && ask < enroll, "the question comes first");
        assert!(link < enroll, "the link answer is offered before the invite");
        assert!(t.contains("I already use Ligus on another device") && t.contains("Yes, start here"));
    }

    /// A box (serve, worker, k8s) joins only through an admission: its
    /// guides never tell it to run `ligus enroll`, and they use admit.
    #[test]
    fn box_guides_never_enroll() {
        for name in ["box", "worker", "kubernetes"] {
            let t = find_topic(name).unwrap().text;
            for line in t.lines() {
                let l = line.trim_start();
                assert!(!(l.contains("ligus enroll") && !l.contains("never") && !l.contains("is for a person") && !l.contains("person's")), "topic {name} tells a box to enroll: {line}");
            }
            assert!(t.contains("admission"), "topic {name} doesn't name the admission");
        }
        for name in ["box", "worker"] {
            let t = find_topic(name).unwrap().text;
            assert!(t.contains("ligus admit request") && t.contains("ligus admit redeem"), "{name}");
        }
        let guide = find_topic("agents-guide").unwrap().text;
        let setup = guide.split("Add an agent").next().unwrap();
        assert!(!setup.lines().any(|l| l.contains("ligus enroll") && !l.contains("never")), "agents-guide box setup enrolls");
        for c in ["serve", "worker", "admit", "admit request", "admit redeem"] {
            assert!(!find_cmd(c).unwrap().examples.iter().any(|x| x.contains("enroll")), "{c} example enrolls");
        }
    }

    #[test]
    fn usage_errors_are_recognised() {
        assert!(is_usage(&usage_error("unknown `ligus remote x`")));
        assert!(is_usage(&anyhow::anyhow!("usage: ligus tail <chat-id>")));
        assert!(!is_usage(&anyhow::anyhow!("unknown chat 123")));
    }

    /// One exit-code table: `ligus help exit-codes` lists 0 to 7, matches the
    /// code's table, and every command's help points to it.
    #[test]
    fn exit_codes_are_one_table() {
        let t = lookup(&["exit-codes"]).unwrap();
        for (code, meaning, _) in crate::problem::TABLE {
            let first = meaning.split([' ', ':', '(']).next().unwrap();
            assert!(t.contains(&format!("  {code}     ")), "exit-codes lacks {code}:\n{t}");
            assert!(t.to_lowercase().contains(&first.to_lowercase()), "exit-codes lacks {meaning:?}");
        }
        assert!(t.contains("--legacy-exit-codes"));
        for c in COMMANDS {
            assert!(command_help(c).contains("ligus help exit-codes"), "ligus {} --help doesn't point to the table", c.name);
        }
        assert!(overview().contains("ligus help exit-codes"));
        assert_eq!((EXIT_OK, EXIT_ERROR, EXIT_USAGE), (crate::problem::EXIT_OK, crate::problem::EXIT_ERROR, crate::problem::EXIT_USAGE));
    }

    /// `ligus help --json` serialises the registry, and the overview keeps
    /// the shared section order.
    #[test]
    fn help_json_and_overview_order() {
        let d = help_json();
        assert_eq!(d["schema"], "ligus.help.v1");
        assert_eq!(d["commands"].as_array().unwrap().len(), COMMANDS.len());
        assert_eq!(d["topics"].as_array().unwrap().len(), TOPICS.len());
        assert_eq!(d["exitCodes"].as_array().unwrap().len(), 8);
        let doctor = d["commands"].as_array().unwrap().iter().find(|c| c["name"] == "doctor").unwrap();
        assert!(doctor["flags"].as_array().unwrap().iter().any(|f| f["flag"] == "--json"));
        let o = overview();
        let mut at = 0;
        for sec in ["NAME", "USAGE", "COMMANDS", "TOPICS", "FLAGS", "EXAMPLES", "ENVIRONMENT", "EXIT CODES", "SEE ALSO"] {
            let i = if sec == "NAME" { o.find("NAME\n") } else { o.find(&format!("\n{sec}")) };
            let i = i.unwrap_or_else(|| panic!("overview lacks {sec}"));
            assert!(i >= at, "overview: {sec} out of order");
            at = i;
        }
        assert!(o.contains("langstroth-relay help") && o.contains("ligus-k8s"), "cross-references to the other tools");
    }
}
