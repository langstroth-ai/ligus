import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn((..._a: unknown[]) => Promise.resolve(undefined));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import * as ipc from "./ipc";
import * as flow from "./flow";
import { setShownWorkspace } from "./shownWorkspace";

// Settings and grant commands name the workspace on screen, so the desktop
// can refuse one that changed since (it acts on that id, never on whatever
// is active by the time the command runs).
const calls: [string, () => Promise<unknown>][] = [
  ["set_attachment_policy", () => ipc.setAttachmentPolicy({} as never)],
  ["block_attachment_hash", () => ipc.blockAttachmentHash("ab")],
  ["unblock_attachment_hash", () => ipc.unblockAttachmentHash("ab")],
  ["set_dearth_mode", () => ipc.setDearthMode(true, "r", [], [])],
  ["approve_join", () => ipc.approveJoin("m")],
  ["set_sandbox_grants", () => ipc.setSandboxGrants({} as never)],
  ["set_sandbox_grants_here", () => ipc.setSandboxGrantsHere(true)],
  ["ack_sandbox_grants", () => ipc.ackSandboxGrants()],
  ["set_workspace_cli_user_config", () => ipc.setWorkspaceCliUserConfig(true)],
  ["set_workspace_egress_presets", () => ipc.setWorkspaceEgressPresets([])],
  ["passkey_remove", () => ipc.passkeyRemove("c")],
  ["workspace_rules_set", () => ipc.workspaceRulesSet(2)],
  ["passkey_regrant", () => ipc.passkeyRegrant("m")],
  ["set_repo_aliases", () => ipc.setRepoAliases("r", [])],
  ["set_host_alias", () => ipc.setHostAlias("a", "h")],
  ["accept_repo_alias", () => ipc.acceptRepoAlias("a", true)],
  ["set_repo_coverage_reporting", () => ipc.setRepoCoverageReporting(null)],
  ["flow_set_zone", () => flow.setZone({ pattern: "src", kind: "protected" as never, note: "" })],
  ["flow_remove_zone", () => flow.removeZone("z")],
];

describe("settings and grant commands", () => {
  beforeEach(() => invoke.mockClear());

  it.each(calls)("%s sends the workspace on screen", async (cmd, call) => {
    setShownWorkspace("ws-shown");
    await call();
    expect(invoke).toHaveBeenCalledTimes(1);
    const [name, args] = invoke.mock.calls[0] as [string, Record<string, unknown>];
    expect(name).toBe(cmd);
    expect(args.workspaceId).toBe("ws-shown");
  });

  it("follows the workspace the window renders", async () => {
    setShownWorkspace("ws-a");
    await ipc.setSandboxGrantsHere(null);
    setShownWorkspace("ws-b");
    await ipc.setSandboxGrantsHere(null);
    expect(invoke.mock.calls.map((c) => (c[1] as { workspaceId: string }).workspaceId)).toEqual(["ws-a", "ws-b"]);
  });
});
