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
