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
      return { name: "Ligus", coreVersion: "2.0.0", buildProfile: "debug" };
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
      return { original: "fn main() {}\n", modified: "fn main() {\n    println!(\"hi\");\n}\n", isBinary: false };
    case "list_agents":
      return fx.agents;
    case "list_members":
      return fx.members;
    case "workspace_members":
      return fx.serverMembers;
    case "presence_list":
      return fx.presence;
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
    case "list_workspace_hosts":
      return fx.hosts;
    case "list_mention_states":
    case "list_all_mention_states":
      return [];
    case "get_workspace_trust":
    case "set_workspace_trust":
    case "trust_workspace":
      return { root: "/mock/repo", items: [] };
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
      return null;
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
        { accountId: "a1b2c3d4-0000-4000-8000-000000000002", displayName: "Maya", avatarUrl: null, safetyCode: "b7e1 04aa 3c9f 6d20 18ee 7f45 a0c3 5b92", keyConflict: false, devices: 2, workspaces: [{ id: "w-bees", name: "Bees" }], dmWorkspaceId: null },
        { accountId: "a1b2c3d4-0000-4000-8000-000000000003", displayName: "Theo", avatarUrl: null, safetyCode: "0c4d 8e21 f9a7 3b56 e102 4d8c 77a9 1f30", keyConflict: false, devices: 1, workspaces: [{ id: "w-bees", name: "Bees" }], dmWorkspaceId: null },
      ];
    case "list_dms":
      return [];
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
      return [{ deviceId: "d1", label: "MacBook Pro", fingerprint: "a1b2-c3d4", thisDevice: true, revoked: false, lastSeen: null }];
    case "list_device_requests":
      return [];
    case "list_relay_users":
      return [];
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
    case "data_dir_migrated":
      return false;
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
