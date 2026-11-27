// Cross-surface navigation. Panes deep inside the rail can't reach App's
// selection state, so they ask for a jump with a window event; App owns the
// switch (and remembers the chat per workspace like a sidebar click does).

export const OPEN_CHAT_EVENT = "ligus:open-chat";

export type OpenChatDetail = { sessionId: string; /** a message to scroll to and flash */ messageId?: string };

export function openChat(sessionId: string, messageId?: string) {
  window.dispatchEvent(new CustomEvent<OpenChatDetail>(OPEN_CHAT_EVENT, { detail: { sessionId, messageId } }));
}

/// Scroll a transcript turn into view and flash it, once it has rendered.
/// Retries for a short while because switching chats renders asynchronously.
export function flashMessage(messageId: string, tries = 20) {
  const el = document.querySelector<HTMLElement>(`.tt[data-message-id="${CSS.escape(messageId)}"]`);
  if (!el) {
    if (tries > 0) window.setTimeout(() => flashMessage(messageId, tries - 1), 60);
    return;
  }
  const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  el.scrollIntoView({ block: "center", behavior: reduce ? "auto" : "smooth" });
  el.classList.remove("ligus-flash");
  void el.offsetWidth;
  el.classList.add("ligus-flash");
}

/// Open the Settings sheet on a tab (e.g. "team-sync") from anywhere; App
/// owns the sheet and closes any modal in front of it.
export const OPEN_SETTINGS_EVENT = "ligus:open-settings";

export function openSettingsTab(tab: string) {
  window.dispatchEvent(new CustomEvent<string>(OPEN_SETTINGS_EVENT, { detail: tab }));
}

/// My devices from anywhere (the palette, People, Set up): open Settings on
/// Profile at My devices, optionally with Add a phone or Add a computer open.
/// `member` is the separate member People thinks is this person on another
/// computer: Add a computer offers to remove it once the computer is added.
/// The devices section reads the request when it mounts and then clears it;
/// one already on screen hears it through DEVICES_REQUEST_EVENT.
export type DevicesRequest = {
  open: "list" | "phone" | "computer";
  member?: { memberId: string; displayName: string };
};

export const DEVICES_REQUEST_EVENT = "ligus:devices-request";

let devicesRequestPending: DevicesRequest | null = null;

export function requestDevices(req: DevicesRequest) {
  devicesRequestPending = req;
  openSettingsTab("account");
  window.dispatchEvent(new CustomEvent<DevicesRequest>(DEVICES_REQUEST_EVENT, { detail: req }));
}

export function pendingDevicesRequest(): DevicesRequest | null {
  return devicesRequestPending;
}

export function clearDevicesRequest() {
  devicesRequestPending = null;
}

export function requestAddPhone() {
  requestDevices({ open: "phone" });
}

/// Open "Admit a box" for the active shared workspace from anywhere (e.g. the
/// add-a-phone check when no box is reachable); App owns the dialog.
export const ADMIT_BOX_EVENT = "ligus:admit-box";

export function openAdmitBox() {
  window.dispatchEvent(new CustomEvent(ADMIT_BOX_EVENT));
}

/// Open a guided setup flow (⌘K "Add an agent", `/setup agent`, an empty
/// pane's action); App owns the wizard.
export const OPEN_SETUP_EVENT = "ligus:open-setup";

export function openSetup(flow: string) {
  window.dispatchEvent(new CustomEvent<string>(OPEN_SETUP_EVENT, { detail: flow }));
}

/// Open the Archived proposals view (Review ⋯, ⌘K) from anywhere; App owns it.
export const OPEN_ARCHIVED_PROPOSALS_EVENT = "ligus:open-archived-proposals";

export function openArchivedProposals() {
  window.dispatchEvent(new CustomEvent(OPEN_ARCHIVED_PROPOSALS_EVENT));
}

/// Open Settings on Sandbox with one agent picked (the agent row's
/// "Sandbox…"). The section reads the request when it mounts and clears it;
/// one already on screen hears it through SANDBOX_AGENT_EVENT.
export const SANDBOX_AGENT_EVENT = "ligus:sandbox-agent";

let sandboxAgentPending: string | null = null;

export function openSandboxForAgent(agentId: string) {
  sandboxAgentPending = agentId;
  openSettingsTab("sandbox");
  window.dispatchEvent(new CustomEvent<string>(SANDBOX_AGENT_EVENT, { detail: agentId }));
}

export function takeSandboxAgentRequest(): string | null {
  const a = sandboxAgentPending;
  sandboxAgentPending = null;
  return a;
}
