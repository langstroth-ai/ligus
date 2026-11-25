// The guided setup flows on the desktop: their names for the palette and the
// composer's `/setup`, and the one action an empty pane can offer. The flows
// themselves (steps, fields, checks) come from the engine
// (`ligus_runtime::setup`) through `setupForm` / `setupRun`.
//
// SETUP_FLOWS mirrors the engine's `FLOWS` (a Rust test in
// app/src/setup_ui.rs checks the ids and titles match).

import type { SetupFlowId } from "@/lib/ipc";
import { openSetup } from "@/lib/nav";

export const SETUP_FLOWS: { id: SetupFlowId; title: string; keywords: string }[] = [
  { id: "agent", title: "Set up an agent", keywords: "agent harness runtime claude codex copilot agy aider pi ollama model" },
  { id: "tool", title: "Connect a tool", keywords: "tool mcp server connect integration" },
  { id: "folder", title: "Add a code folder", keywords: "folder repo repository git checkout bind code" },
  { id: "box", title: "Add a box", keywords: "box server serve worker home always-on admit admission" },
  { id: "phone", title: "Add a phone", keywords: "phone iphone mobile pair qr device" },
  { id: "invite", title: "Invite a person", keywords: "invite person people member teammate join role" },
  { id: "relay", title: "Set up a relay", keywords: "relay sync server hosted self-hosted community https" },
];

export function isSetupFlow(id: string): id is SetupFlowId {
  return SETUP_FLOWS.some((f) => f.id === id);
}

/// `/setup`, `/setup agent`, `/setup tool …` typed in the composer: the flow
/// to open (`null` for the bare command, which lists them), or `undefined`
/// when the text isn't a setup command.
export function parseSetupCommand(text: string): SetupFlowId | null | undefined {
  const m = /^\/setup(?:\s+(\S+))?\s*$/.exec(text.trim());
  if (!m) return undefined;
  if (!m[1]) return null;
  return isSetupFlow(m[1]) ? m[1] : undefined;
}

/// The composer's `/setup` completions for what is typed after the slash.
export function setupSlashItems(query: string): { id: string; flow: SetupFlowId; label: string }[] {
  const q = query.toLowerCase();
  return SETUP_FLOWS.filter((f) => `setup ${f.id} ${f.title} ${f.keywords}`.toLowerCase().includes(q) || q === "setup").map((f) => ({
    id: `setup-${f.id}`,
    flow: f.id,
    label: f.title,
  }));
}

/// The one action an empty pane offers: its flow's title and a click that
/// opens it. Panes render it as their EmptyHint's action.
export function setupAction(flow: SetupFlowId): { label: string; onClick: () => void } {
  const f = SETUP_FLOWS.find((x) => x.id === flow)!;
  return { label: f.title, onClick: () => openSetup(flow) };
}
