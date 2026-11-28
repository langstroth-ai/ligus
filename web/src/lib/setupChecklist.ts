// "Set up Ligus": what each checklist row's one action does on the desktop,
// and how a Clean up item is carried out. The rows themselves come from the
// backend (`ligus_runtime::setup::checklist`), the same on every surface;
// nothing here decides a row's state.

import {
  removeAgent,
  removeMember,
  revokeMyDevice,
  setupHide,
  setupRemoveHost,
  type CleanupItem,
  type SetupRowState,
  type UnpairedPhoneDto,
} from "@/lib/ipc";
import { loadHiddenLeft, saveHiddenLeft } from "@/lib/lifecycle";
import { loadHiddenRevoked, saveHiddenRevoked } from "@/lib/revokedDevices";
import { openSettingsTab, openSetup, requestAddPhone, requestDevices } from "@/lib/nav";
import { shownWorkspace } from "@/lib/shownWorkspace";

/// The mark a row reads as: ✓ done, ○ not set up (grey), • wants a look (amber).
export function rowMark(state: SetupRowState): string {
  return state === "done" ? "✓" : state === "notSetUp" ? "○" : "•";
}

/// The colour of a row's mark. Never red: something never chosen is grey.
export function rowTone(state: SetupRowState): string {
  return state === "done" ? "var(--ligus-success-ink)" : state === "attention" ? "var(--ligus-warn-ink)" : "var(--ligus-ink-faint)";
}

/// The one line a box upgrade needs, shown in place of a link.
export const BOX_UPGRADE_LINE = "ligus-local upgrade";

export type RowActionTarget =
  | { kind: "settings"; tab: string }
  | { kind: "devices"; open: "list" | "computer" | "phone" }
  | { kind: "setup"; flow: string }
  | { kind: "cleanup" }
  | { kind: "boxUpgrade" }
  | { kind: "none" };

/// Where a row's action goes on the desktop.
export function rowActionTarget(id: string): RowActionTarget {
  switch (id) {
    case "profile":
      return { kind: "settings", tab: "account" };
    case "passkey":
    case "recovery":
    case "upgradeRules":
      return { kind: "settings", tab: "passkeys" };
    case "deviceName":
    case "devices":
      return { kind: "devices", open: "list" };
    case "addComputer":
      return { kind: "devices", open: "computer" };
    case "addPhone":
      return { kind: "devices", open: "phone" };
    case "addBox":
      return { kind: "setup", flow: "box" };
    case "addAgent":
    case "agents":
      return { kind: "setup", flow: "agent" };
    case "upgradeBox":
      return { kind: "boxUpgrade" };
    case "cleanup":
      return { kind: "cleanup" };
    default:
      // "push" is the phone's own switch: nothing to do here.
      return { kind: "none" };
  }
}

/// Run a navigation target (the in-pane ones, Clean up and the box line, are
/// the pane's own).
export function goTo(t: RowActionTarget) {
  if (t.kind === "settings") openSettingsTab(t.tab);
  else if (t.kind === "devices") {
    if (t.open === "phone") requestAddPhone();
    else requestDevices({ open: t.open });
  } else if (t.kind === "setup") openSetup(t.flow);
}

/// Carry out a Clean up item's action, after the pane's confirm. A removal
/// goes through the existing command (each its own passkey ceremony); Remove
/// from view writes nothing in the workspace and also hides the entry where
/// People and My devices keep their own "hidden" lists.
export async function runCleanup(item: CleanupItem): Promise<void> {
  if (item.action.id === "removeFromView") {
    await setupHide(item.key);
    if (item.kind === "memberLeft" || item.kind === "memberRetired") {
      const s = loadHiddenLeft();
      s.add(item.id);
      saveHiddenLeft(s);
    } else if (item.kind === "deviceRevoked") {
      const s = loadHiddenRevoked();
      s.add(item.id);
      saveHiddenRevoked(s);
    }
    return;
  }
  // The session argument is only a routing hint for these commands.
  const session = shownWorkspace();
  switch (item.kind) {
    case "memberLeft":
      return removeMember(session, item.id);
    case "devicePending":
    case "deviceIdle":
    case "unpairedPhone":
      return revokeMyDevice(item.id);
    case "formerHost":
      return setupRemoveHost(item.id);
    case "legacyAgent":
      return removeAgent(session, item.id);
    default:
      throw new Error("This can only be removed from view.");
  }
}

/// The confirm for removing several at once: each still asks for its passkey,
/// one after another ("Confirm 1 of 3").
export function batchConfirm(items: CleanupItem[]): string {
  const n = items.length;
  const names = items.map((i) => i.label).join(", ");
  return `Remove ${n === 1 ? "this one" : `these ${n}`} from the workspace: ${names}? Each asks for your passkey in turn, and none can be undone.`;
}

/// Your phones a box unpaired, or that forgot a box, as Clean up items. They
/// come from the boxes' reports (the relay), so the pane asks for them only
/// when Clean up is open; the checklist row itself stays local.
export function unpairedItems(phones: UnpairedPhoneDto[], have: CleanupItem[]): CleanupItem[] {
  const seen = new Set(have.map((i) => i.key));
  const out: CleanupItem[] = [];
  for (const p of phones) {
    const key = `unpairedPhone:${p.deviceId}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const box = p.hostLabel.trim() || "a box";
    const name = p.name.trim() || "your phone";
    out.push({
      key,
      kind: "unpairedPhone",
      id: p.deviceId,
      label: name,
      detail: p.by === "phone" ? `forgot ${box}` : `unpaired from ${box}`,
      action: {
        id: "remove",
        label: "Remove",
        passkey: true,
        confirm: `Remove the phone ${name} from your devices? It can't be undone; add it again to use it. It asks for your passkey.`,
      },
    });
  }
  return out;
}
