// Dev-only: the guided setup flows as the engine (`ligus_runtime::setup`)
// shapes them, with demo data, for previews and screenshots.

// "Set up Ligus": the backend's own fixture rows.
import settledChecklist from "../../../crates/ligus-runtime/tests/fixtures/setup-checklist/settled-team.checklist.json";

type Args = Record<string, unknown> | undefined;

const flows = [
  { id: "agent", title: "Set up an agent", summary: "an agent on a coding CLI or model that is installed and signed in", why: "An agent answers when you @mention it. It runs one harness (Claude Code, Codex, Copilot, Antigravity, Aider, Pi or a local model) inside this device's agent sandbox.", keywords: "", minRole: "Contributor", personSteps: false },
  { id: "tool", title: "Connect a tool", summary: "an MCP server your agents can call", why: "A tool is an MCP server: a command on this device or an https URL. Its token stays in this device's secret store, and you choose which agents may reach it.", keywords: "", minRole: null, personSteps: false },
  { id: "folder", title: "Add a code folder", summary: "a git checkout on this device, bound to a workspace repository", why: "Agents work on a repository in its own worktree per chat. Binding tells this device which folder holds the workspace's repository.", keywords: "", minRole: "Contributor", personSteps: false },
  { id: "box", title: "Add a box", summary: "an always-on machine (ligus serve) that joins as its own member", why: "A box keeps agents working while your laptop sleeps. It joins only through an admission an Admin approves with a passkey.", keywords: "", minRole: null, personSteps: true },
  { id: "phone", title: "Add a phone", summary: "pair your phone with this workspace", why: "The phone app follows your chats and approvals. Pairing happens in your own window; the pairing secret is never logged.", keywords: "", minRole: null, personSteps: true },
  { id: "invite", title: "Invite a person", summary: "invite someone to this workspace and see when they join", why: "An invite is single use and carries the workspace key: send it only to the person it is for. Their role decides what they may do.", keywords: "", minRole: "Admin", personSteps: true },
  { id: "relay", title: "Set up a relay", summary: "the relay this workspace syncs through (community or your own, https only)", why: "The relay carries end-to-end encrypted events between devices; it never sees the workspace key. Use the community relay or run your own.", keywords: "", minRole: null, personSteps: false },
];

const ok = (id: string, title: string, detail: string) => ({ id, title, status: "ok", detail });

function models(harness: string) {
  if (harness === "claude-code")
    return [
      { value: "sonnet", label: "Sonnet" },
      { value: "opus", label: "Opus" },
      { value: "haiku", label: "Haiku" },
    ];
  if (harness === "codex") return [{ value: "gpt-5.3-codex", label: "gpt-5.3-codex" }];
  return [];
}

function steps(flow: string, inputs: Record<string, string>) {
  switch (flow) {
    case "agent":
      return [
        {
          id: "harness",
          title: "Harness",
          kind: "input",
          fields: [
            {
              id: "harness",
              label: "Harness",
              type: "choice",
              required: true,
              help: "The coding CLI or model the agent runs. It must be installed and signed in on this device.",
              options: [
                { value: "claude-code", label: "Claude Code", hint: "signed in" },
                { value: "codex", label: "Codex", hint: "signed in" },
                { value: "copilot", label: "GitHub Copilot CLI", hint: "not installed" },
                { value: "antigravity", label: "Google Antigravity CLI", hint: "installed, sign-in unknown" },
                { value: "aider", label: "Aider", hint: "not installed" },
                { value: "pi", label: "Pi", hint: "not installed" },
                { value: "local", label: "Local model (Ollama or OpenAI-compatible)" },
                { value: "gateway", label: "Model gateway on a host" },
              ],
            },
            { id: "endpoint", label: "Endpoint", type: "text", required: false, default: "http://localhost:11434", when: ["harness", ["local"]], help: "The model server's URL." },
          ],
        },
        { id: "name", title: "Name", kind: "input", fields: [{ id: "name", label: "Name", type: "text", required: true, help: "You @mention it by this name." }] },
        { id: "model", title: "Model", kind: "input", fields: [{ id: "model", label: "Model", type: "suggest", required: false, options: models(inputs.harness ?? ""), help: "Leave it empty for the harness's own default." }] },
        { id: "test", title: "Test", kind: "input", fields: [{ id: "test-turn", label: "Run a test turn", type: "toggle", required: false, help: "Sends one short message and checks the reply." }] },
        { id: "verify", title: "Ready", kind: "verify" },
      ];
    case "tool":
      return [
        {
          id: "server",
          title: "Server",
          kind: "input",
          fields: [
            { id: "id", label: "Name", type: "text", required: true, help: "A short id for the server." },
            { id: "kind", label: "Kind", type: "choice", required: true, default: "command", options: [{ value: "command", label: "A command on this device" }, { value: "url", label: "A URL (https)" }] },
            { id: "command", label: "Command", type: "text", required: true, when: ["kind", ["command"]], help: "The command and its arguments. Never put a token here." },
            { id: "url", label: "URL", type: "text", required: true, when: ["kind", ["url"]] },
          ],
        },
        { id: "token", title: "Token", kind: "input", fields: [{ id: "token", label: "Bearer token", type: "secret", required: false, when: ["kind", ["url"]], help: "Only if the server needs one." }] },
        {
          id: "reach",
          title: "Agents",
          kind: "input",
          fields: [
            {
              id: "reach",
              label: "Agents that may use it",
              type: "multi",
              required: false,
              default: "all",
              options: [
                { value: "all", label: "Every agent" },
                { value: "builder", label: "@builder" },
                { value: "reviewer", label: "@reviewer" },
              ],
            },
          ],
        },
        { id: "verify", title: "Connected", kind: "verify" },
      ];
    case "folder":
      return [
        { id: "folder", title: "Folder", kind: "input", fields: [{ id: "path", label: "Folder", type: "path", required: true, help: "A git checkout on this device." }] },
        {
          id: "repo",
          title: "Repository",
          kind: "input",
          fields: [
            { id: "name", label: "Display name", type: "text", required: false },
            { id: "branch", label: "Default branch", type: "text", required: false },
          ],
        },
        { id: "verify", title: "Bound", kind: "verify" },
      ];
    case "box":
      return [{ id: "admit", title: "Admit the box", kind: "person", where: "Admit a box" }, { id: "verify", title: "Box joined", kind: "verify" }];
    case "phone":
      return [{ id: "pair", title: "Pair", kind: "person", where: "Add a phone" }, { id: "verify", title: "Paired", kind: "verify" }];
    case "invite":
      return [{ id: "create", title: "Create the invite", kind: "person", where: "this window" }, { id: "track", title: "Accepted", kind: "verify" }];
    default:
      return [
        {
          id: "relay",
          title: "Relay",
          kind: "input",
          fields: [
            {
              id: "kind",
              label: "Relay",
              type: "choice",
              required: true,
              default: "community",
              options: [
                { value: "community", label: "Community relay", hint: "relay.langstroth.ai, best-effort" },
                { value: "own", label: "Your own relay", hint: "langstroth-relay on a server you run" },
              ],
            },
            { id: "url", label: "URL", type: "text", required: true, when: ["kind", ["own"]] },
          ],
        },
        { id: "token", title: "Access token", kind: "input", fields: [{ id: "token", label: "Access token", type: "secret", required: false }] },
        { id: "verify", title: "Connected", kind: "verify" },
      ];
  }
}

function preflight(flow: string) {
  if (flow === "agent") return [ok("agent.sandbox", "agent sandbox", "seatbelt")];
  if (flow === "tool") return [ok("tool.sandbox", "agent sandbox", "seatbelt")];
  if (flow === "phone")
    return [{ id: "phone.precheck.noBoxOnline", title: "phone", status: "warn", detail: "your box hasn't reported in for 20 minutes" }];
  return [];
}

function outcome(flow: string, inputs: Record<string, string>) {
  const base = { schema: "ligus.checks.v1", tool: "ligus", command: `setup ${flow}`, flow, dryRun: false, results: {}, next: [] as string[] };
  switch (flow) {
    case "agent": {
      const name = inputs.name || "builder";
      return {
        ...base,
        ok: true,
        exitCode: 0,
        done: true,
        checks: [
          ok("agent.sandbox", "agent sandbox", "seatbelt"),
          ok("agent.harness.installed", "Claude Code", "installed 2.1.331"),
          ok("agent.harness.signed-in", "sign-in", "signed in (credentials found)"),
          ok("agent.name", "name", `@${name}`),
          ok("agent.model", "model", inputs.model || "the harness's default"),
          ok("agent.runtime", "runtime", "reusing 'claude-code'"),
          ok("agent.agent", "agent", `@${name} created`),
          ...(inputs["test-turn"] === "true" ? [ok("agent.test-turn", "test turn", "answered in 1240 ms")] : []),
        ],
      };
    }
    case "tool":
      return {
        ...base,
        ok: true,
        exitCode: 0,
        done: true,
        checks: [ok("tool.sandbox", "agent sandbox", "seatbelt"), ok("tool.server", "server", `${inputs.id || "notes"}`), ok("tool.handshake", "handshake", "11 tools"), ok("tool.write", "save", `'${inputs.id || "notes"}' for @builder`)],
      };
    case "box":
      return {
        ...base,
        ok: true,
        exitCode: 4,
        done: false,
        checks: [{ id: "box.admit", title: "admit", status: "warn", detail: "on the box, run `ligus setup box`; it prints a join request", fix: "open Admit a box, paste the request, check the fingerprint and approve with a passkey" }],
      };
    case "phone":
      return {
        ...base,
        ok: true,
        exitCode: 4,
        done: false,
        checks: [{ id: "phone.pair", title: "pair", status: "warn", detail: "pairing shows a code only in this window", fix: "open Add a phone and scan the code with the phone app" }],
      };
    case "invite":
      return {
        ...base,
        ok: true,
        exitCode: 4,
        done: false,
        checks: [
          ok("invite.role", "role", "They join as a Contributor: they chat, run agents and propose changes. An Admin can change the role in People."),
          ok("invite.track", "invites", "1 waiting, 2 accepted"),
          { id: "invite.create", title: "invite", status: "warn", detail: "the code is shown once, in this window, to copy", fix: "create the invite here and send it to the person" },
        ],
      };
    case "folder":
      return {
        ...base,
        ok: false,
        exitCode: 2,
        done: false,
        checks: [{ id: "folder.folder.git", title: "git", status: "fail", detail: "that folder is a linked worktree", fix: "pick its main checkout (git worktree list shows it)" }],
      };
    default:
      return {
        ...base,
        ok: true,
        exitCode: 0,
        done: true,
        checks: [ok("relay.check", "relay", "https://relay.langstroth.ai: reachable, access accepted"), ok("relay.switch", "workspace", "this device now syncs through https://relay.langstroth.ai"), { id: "relay.members", title: "members", status: "warn", detail: "every member's devices must use the same relay; new invites carry this one", fix: "on each device: Set up a relay (ligus setup relay)" }],
      };
  }
}

export function setupMock(cmd: string, args: Args): { handled: boolean; value?: unknown } {
  const flow = (args?.flow as string) ?? "agent";
  const inputs = (args?.inputs as Record<string, string>) ?? {};
  switch (cmd) {
    case "setup_flows":
      return { handled: true, value: flows };
    case "setup_status":
      return { handled: true, value: flows.map((f) => ({ flow: f, progress: f.id === "agent" ? "done" : "todo", detail: "" })) };
    case "setup_form": {
      const def = flows.find((f) => f.id === flow)!;
      return { handled: true, value: { flow: def, preflight: preflight(flow), blocked: false, steps: steps(flow, inputs) } };
    }
    case "setup_run":
      return { handled: true, value: outcome(flow, inputs) };
    case "setup_cancel":
      return { handled: true, value: null };
    case "setup_checklist":
      return { handled: true, value: settledChecklist };
    case "setup_hide":
    case "setup_remove_host":
      return { handled: true, value: null };
    case "workspace_invite":
      return { handled: true, value: "hivews1:eyJkZW1vIjoiZGVtbyBpbnZpdGUsIG5vdCBhIHJlYWwga2V5In0" };
    case "pick_workspace_folder":
      return { handled: true, value: "/Users/demo/src/hive-garden" };
    default:
      return { handled: false };
  }
}
