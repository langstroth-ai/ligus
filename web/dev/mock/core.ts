// Dev-only stand-in for @tauri-apps/api/core (see vite.mock.config.ts): a
// canned backend so the UI can be previewed and screenshotted in a browser.
// Never imported by the production build.
//
// Scenario knobs (URL query): ?scenario=onboarding|empty|default
//   &dearth=1 (dearth mode on)  &lockdown=1 (open chat locked down)
//   &loading=1 (every call hangs) &error=1 (every list call fails)
import * as fx from "./fixtures";

const params = new URLSearchParams(window.location.search);
const scenario = params.get("scenario") ?? "default";
const flags = {
  dearth: params.get("dearth") === "1",
  lockdown: params.get("lockdown") === "1",
  loading: params.get("loading") === "1",
  error: params.get("error") === "1",
  empty: scenario === "empty" || scenario === "onboarding",
};

type Args = Record<string, unknown> | undefined;

function handle(cmd: string, args: Args): unknown {
  const sid = (args?.sessionId as string) ?? "";
  switch (cmd) {
    case "get_app_info":
      return { name: "Ligus", coreVersion: "0.1.0", buildProfile: "debug" };
    case "get_app_settings":
      return fx.settings(scenario === "onboarding");
    case "list_workspaces":
      return fx.workspaces;
    case "list_runtimes":
      return fx.runtimes;
    case "list_chats":
      return flags.empty ? [] : fx.chats;
    case "list_channels":
      return flags.empty ? [] : fx.channels;
    case "get_chat":
      return flags.empty ? null : fx.chatSession(sid);
    case "get_context_telemetry":
      return flags.empty ? null : fx.telemetry(sid);
    case "sync_status":
      return fx.syncStatus;
    case "get_git_status":
      return { branch: "main", dirtyCount: 3 };
    case "get_workspace_diffs":
      return flags.empty ? [] : fx.diffs;
    case "get_file_diff_sides":
      return fx.fileDiffSides(String(args?.path ?? ""));
    case "list_workspace_repos":
      return fx.workspaceRepos;
    case "memory_view":
      return fx.memoryView(String(args?.query ?? ""));
    case "task_state":
      return [];
    case "list_agents":
      return fx.agents;
    case "set_agent_access":
      return null;
    case "get_agent_usage":
      return fx.agentUsage(String(args?.agentId ?? ""));
    case "list_members":
      return fx.members;
    case "workspace_members":
      return fx.serverMembers;
    case "presence_list":
      return fx.presence;
    case "get_share_typing":
      return true;
    case "set_share_typing":
      return null;
    case "get_primary_open_to_members":
      return false;
    case "set_primary_open_to_members":
      return null;
    case "list_proposals":
      return flags.empty ? [] : fx.proposals;
    case "list_proposal_sets":
      return flags.empty ? [] : fx.proposalSets;
    case "list_skills":
      return fx.skills;
    case "list_mcp_servers":
      return fx.mcpServers;
    case "list_vaults":
      return fx.vaults;
    case "list_workflows":
      return fx.workflows;
    case "list_workflow_runs":
      return fx.workflowRuns;
    case "list_queued_work":
      return fx.queuedWork;
    case "list_chat_attention":
      return flags.empty ? [] : fx.chatAttention;
    case "list_workspace_hosts":
      return fx.hosts;
    case "list_mention_states":
    case "list_all_mention_states":
      return [];
    case "get_workspace_trust":
    case "set_workspace_trust":
    case "trust_workspace":
      return { root: "/mock/repo", items: [] };
    case "get_sandbox_policy":
      return {
        policies: [
          {
            runtime: "ClaudeCode",
            tier: "seatbelt",
            home: "/Users/mock",
            entries: [
              { path: "<the turn's worktree>", access: "read-write", why: "the turn's own checkout (.git and the protected config read-only)" },
              { path: "<the turn's scratch dir and home>", access: "read-write", why: "TMPDIR and HOME for this turn; removed when it ends" },
              { path: "/Users/mock/.cargo/bin", access: "read", why: "toolchain: rustup proxies and cargo-installed tools" },
              { path: "/Users/mock", access: "denied", why: "your home dir: everything not listed above" },
              { path: "/Users/mock/.ssh", access: "denied", why: "credentials (never opened by a signed grant)" },
            ],
            shell: "the workspace Owner's turns only (the device owner's in the local workspace); other members' turns run without it",
          },
        ],
        grants: {},
        agents: [{ id: "00000000-0000-4000-8000-0000000000a1", name: "coder", runtime: "ClaudeCode" }],
        canEditPaths: true,
        canEditShell: true,
        local: false,
        appliesHere: true,
        appliesByDefault: true,
        appliesSetting: null,
        appliesWhy: "applied: this device is the workspace Owner's (the default)",
        changed: false,
      };
    case "set_sandbox_grants":
    case "set_sandbox_grants_here":
    case "ack_sandbox_grants":
      return null;
    case "get_dearth_state":
      return fx.dearth(flags.dearth);
    case "get_egress_presets":
      return {
        available: [
          {
            name: "package-registries",
            label: "Package registries",
            hosts: ["registry.npmjs.org", "crates.io", "static.crates.io", "index.crates.io", "pypi.org", "files.pythonhosted.org", "github.com", "api.github.com", "codeload.github.com", "objects.githubusercontent.com", "raw.githubusercontent.com"],
          },
          { name: "claude-connectors", label: "Claude connectors", hosts: ["mcp-proxy.anthropic.com"] },
        ],
        workspace: [],
        chat: null,
        host: [],
        restrictedBy: flags.lockdown ? "lockdown" : flags.dearth ? "dearth" : "",
      };
    case "set_workspace_egress_presets":
    case "set_chat_egress_presets":
    case "set_workspace_cli_user_config":
      return null;
    case "get_workspace_cli_user_config":
      return false;
    case "get_chat_lockdown":
      return fx.lockdown(flags.lockdown);
    case "list_guard_signals":
      return fx.guardSignals;
    case "get_hygiene_report":
    case "run_hygiene_now":
      return fx.hygiene;
    case "get_throttle_state":
      return fx.throttle;
    case "growth_suggestions":
      return fx.growth;
    case "flow_get_settings":
      return fx.flowSettings;
    case "flow_set_settings":
      return args?.settings;
    case "flow_handoffs":
      return flags.empty ? [] : fx.handoffs;
    case "flow_routing_report":
      return fx.routing;
    case "flow_queue_report":
      return fx.queue;
    case "flow_marks":
      return fx.marks;
    case "flow_zones":
      return fx.zones;
    case "flow_roster":
      return fx.roster;
    case "flow_board":
      return fx.board;
    case "flow_home":
      return fx.home;
    case "get_connection_settings":
      return fx.connection;
    case "get_agent_tool_settings":
      return { enabled: true, writeInPlace: false, runCommand: true };
    case "get_retrieval_settings":
      return { embedUrl: "", embedModel: "", rerankUrl: "", rerankModel: "", topK: 8, candidates: 40 };
    case "get_context_commands":
      return { summarizePrompt: "", compactPrompt: "", defaultPrompt: "Summarize the conversation so far." };
    case "get_utility_runtime":
      return null;
    case "list_providers":
      return fx.providers;
    case "list_provider_presets":
      return [];
    case "detect_cli_runtimes":
      return fx.clis;
    case "detect_providers":
      return [];
    case "detect_environment":
      return { claudeCode: true, codex: false, ollama: true, anthropicEnv: false, openaiEnv: false, gitName: "Zaz", gitEmail: "zaz@example.com" };
    case "list_agent_templates":
      return [];
    case "my_profile":
      return { accountId: "6f1c2d3e-8a7b-4c1d-9e0f-112233445566", displayName: "Zaz", avatarUrl: null, safetyCode: "3f2a 91c0 77de 0b14 c2e8 5a6f 9d31 e047", gitEmail: "zaz@example.com" };
    case "list_co_members":
      return [
        { accountId: "a1b2c3d4-0000-4000-8000-000000000002", displayName: "Maya", avatarUrl: null, safetyCode: "b7e1 04aa 3c9f 6d20 18ee 7f45 a0c3 5b92", keyConflict: false, devices: 2, workspaces: [{ id: "w-bees", name: "Bees" }], dmWorkspaceId: null, links: [{ provider: "forgejo", host: "codeberg.org", handle: "maya", subjectId: "48213", status: null }] },
        { accountId: "a1b2c3d4-0000-4000-8000-000000000003", displayName: "Theo", avatarUrl: null, safetyCode: "0c4d 8e21 f9a7 3b56 e102 4d8c 77a9 1f30", keyConflict: false, devices: 1, workspaces: [{ id: "w-bees", name: "Bees" }], dmWorkspaceId: null },
      ];
    case "list_dms":
      return [];
    case "my_identity_links":
      return [
        { linkId: "l-1", provider: "github", host: "github.com", handle: "zaz", subjectId: "1234567", method: "sshSig", issuedAt: "2026-11-14T12:00:00Z", expiresAt: "2027-11-14T12:00:00Z", workspaces: [{ id: "w-bees", name: "Bees" }], discoverable: false, status: { state: "verified", checkedAt: "2026-11-14T12:00:00Z", handle: "zaz" } },
      ];
    case "identity_link_statuses":
      return [
        { linkId: "l-2", provider: "forgejo", host: "codeberg.org", handle: "maya", subjectId: "48213", method: "sshSig", issuedAt: "2026-11-01T12:00:00Z", expiresAt: "2027-11-01T12:00:00Z", workspaces: [{ id: "w-bees", name: "Bees" }], discoverable: false, status: { state: "verified", checkedAt: "2026-11-14T12:00:00Z", handle: "maya" } },
      ];
    case "identity_link_settings":
      return { checkForges: true, forges: [] };
    case "list_pending_invites":
      return { contacts: "on", invites: [] };
    case "list_schedules":
      return fx.schedules;
    case "list_claude_code_models":
      return [{ value: "default", label: "Default", description: "Recommended" }, { value: "opus", label: "Opus", description: null }];
    case "get_claude_code_model":
      return "default";
    case "detect_editors":
      return [{ id: "vscode", label: "VS Code" }];
    case "launch_health":
      return { macos: true, translocated: false, quarantined: false };
    case "list_membership_audit":
      return [];
    case "workspace_list_invites":
      return [];
    case "list_my_devices":
      return [
        { deviceId: "d1", label: "MacBook Pro", fingerprint: "a1b2-c3d4", thisDevice: true, revoked: false, lastSeen: null, phone: false, named: true },
        { deviceId: "p1", label: "Ana's iPhone", fingerprint: "e5f6-a7b8", thisDevice: false, revoked: false, lastSeen: null, phone: true, named: true },
      ];
    case "list_device_pairings":
      return [{ deviceId: "p1", hostId: "h1", hostLabel: "home-server", name: "Ana's iPhone", lastSeen: Math.floor(Date.now() / 1000) - 120, reportedAt: Math.floor(Date.now() / 1000) }];
    case "list_device_requests":
      return [];
    case "list_device_revocations":
      return [];
    case "retry_device_revocation":
      return null;
    case "has_issuer_key":
      return false;
    case "p2p_my_code":
      return "LIG-7Q2-XK9";
    case "p2p_list_contacts":
      return [];
    case "lsp_servers":
      return [];
    case "list_workspace_tree":
      return [{ name: "src", path: "src", isDir: true }, { name: "README.md", path: "README.md", isDir: false }];
    case "list_workspace_files":
      return ["README.md", "src/main.rs", "src/lib.rs"];
    case "read_workspace_file":
      return "# Hello\n";
    case "read_recent_logs":
      return "2026-11-13T10:00:00Z INFO ligus: started\n";
    case "check_for_update":
    case "check_for_app_update":
      return null;
    case "export_config":
      return "";
    case "endpoint_is_private":
      return true;
    case "ollama_list_models":
      return { base: "http://localhost:11434", host: "localhost", models: [], pulling: [] };
    case "model_profile_hint":
      return null;
    case "forges_list":
      return [
        { id: "github:github.com", kind: "github", flavor: null, displayName: "GitHub", baseUrl: "https://github.com", authSource: "gh", hasRead: true, hasWrite: true, user: "ana", deviceFlow: false, openLabel: "Open draft PR", holder: "this device", available: true },
        { id: "gitea:codeberg.org", kind: "gitea", flavor: "codeberg", displayName: "Codeberg", baseUrl: "https://codeberg.org", authSource: "token", hasRead: true, hasWrite: false, user: null, deviceFlow: false, openLabel: "Open WIP PR", holder: "this device" },
      ];
    case "forges_repos":
      return { login: "ana", owners: ["ana"], repos: [{ repo: { owner: "ana", name: "hive-notes" }, description: "Bee notes", private: false, defaultBranch: "main", updatedAt: "2026-11-01", webUrl: "https://github.com/ana/ligus-notes", fork: false }], hasMore: false };
    case "request_apply_on_host":
      return "mock-apply-request";
    case "withdraw_apply_request":
      return null;
    case "phone_enroll_precheck":
      // ?phone=nobox shows the add-a-phone check with nothing to go through.
      return params.get("phone") === "nobox"
        ? {
            ready: false,
            missing: ["No box in this workspace. The phone reaches you through an always-on box (ligus serve)."],
            reasons: [
              { code: "noBox", text: "No box in this workspace. The phone reaches you through an always-on box (ligus serve).", blocking: true },
              { code: "noPasskey", text: "No passkey or recovery key for your account here. Adding a phone is approved with one.", blocking: true },
            ],
          }
        : {
            ready: true,
            missing: [],
            reasons: [
              { code: "noBoxOnline", text: "No box reachable right now: serve box was last seen 2 min ago. The phone's request goes through it, so start it first.", blocking: false },
            ],
          };
    default:
      if (cmd.startsWith("list_")) return [];
      return null;
  }
}

export async function invoke<T>(cmd: string, args?: Args): Promise<T> {
  if (flags.loading && !["get_app_settings", "list_workspaces", "list_runtimes"].includes(cmd)) {
    return new Promise<T>(() => {});
  }
  if (flags.error && /^(list_|get_workspace_diffs|flow_)/.test(cmd)) {
    throw new Error(`${cmd}: relay unreachable (mock)`);
  }
  await new Promise((r) => setTimeout(r, 15));
  return structuredClone(handle(cmd, args)) as T;
}

export function convertFileSrc(path: string): string {
  return path;
}

export function isTauri(): boolean {
  return false;
}

export class Channel<T> {
  onmessage: (m: T) => void = () => {};
}
