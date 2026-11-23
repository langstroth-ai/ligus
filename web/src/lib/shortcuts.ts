// The app's keyboard shortcuts: one list that both the key handler (App) and
// the shortcuts sheet (⌘/) read, so the sheet can't drift from what the keys
// really do. Bindings handled inside a view (the composer, the code view) are
// listed here too, marked with where they work.

import { PANES } from "@/lib/panes";

export type ShortcutId =
  | "palette"
  | "shortcuts"
  | "settings"
  | "composer"
  | "nextChat"
  | "prevChat"
  | "toggleSidebar"
  | "toggleTools"
  | "pane"
  | "zoomIn"
  | "zoomOut"
  | "zoomReset"
  // Handled by a view, not the global handler:
  | "send"
  | "newline"
  | "stopTurn"
  | "quickOpen";

export type ShortcutGroup = "General" | "Chat" | "Panes" | "View" | "Code";

export interface Shortcut {
  id: ShortcutId;
  /// Display form, Mac symbols (⌘ ⌥ ⇧); see `keyLabel` for other platforms.
  keys: string;
  label: string;
  group: ShortcutGroup;
  /// The pane a "pane" shortcut opens.
  pane?: string;
}

/// ⌘⌥1…9 open the first nine panes and ⌘⌥0 the tenth.
export const PANE_DIGITS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];

export const SHORTCUTS: Shortcut[] = [
  { id: "palette", keys: "⌘K", label: "Command palette", group: "General" },
  { id: "shortcuts", keys: "⌘/", label: "Keyboard shortcuts", group: "General" },
  { id: "settings", keys: "⌘,", label: "Settings", group: "General" },
  { id: "composer", keys: "⌘L", label: "Focus the message box", group: "Chat" },
  { id: "send", keys: "↵", label: "Send (in the message box)", group: "Chat" },
  { id: "newline", keys: "⇧↵", label: "New line (in the message box)", group: "Chat" },
  { id: "stopTurn", keys: "Esc", label: "Stop the running turn (outside a box with text in it)", group: "Chat" },
  { id: "nextChat", keys: "⌘⇧]", label: "Next chat", group: "Chat" },
  { id: "prevChat", keys: "⌘⇧[", label: "Previous chat", group: "Chat" },
  ...PANES.slice(0, PANE_DIGITS.length).map(
    (p, i): Shortcut => ({ id: "pane", keys: `⌘⌥${PANE_DIGITS[i]}`, label: `Open ${p.label}`, group: "Panes", pane: p.id }),
  ),
  { id: "toggleSidebar", keys: "⌘B", label: "Show or hide the sidebar", group: "View" },
  { id: "toggleTools", keys: "⌘J", label: "Show or hide the tools rail", group: "View" },
  { id: "zoomIn", keys: "⌘+", label: "Zoom in", group: "View" },
  { id: "zoomOut", keys: "⌘−", label: "Zoom out", group: "View" },
  { id: "zoomReset", keys: "⌘0", label: "Actual size", group: "View" },
  { id: "quickOpen", keys: "⌘P", label: "Quick open a file (in Code)", group: "Code" },
];

/// The key hint for a pane, or undefined when it has no key.
export function paneKey(paneId: string): string | undefined {
  return SHORTCUTS.find((s) => s.id === "pane" && s.pane === paneId)?.keys;
}

/// The key hint for a shortcut id.
export function shortcutKey(id: ShortcutId): string | undefined {
  return SHORTCUTS.find((s) => s.id === id)?.keys;
}

export function isMac(): boolean {
  if (typeof navigator === "undefined") return true;
  return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

/// Mac symbols as written, or "Ctrl+Alt+Shift+" spelled out elsewhere.
export function keyLabel(keys: string, mac = isMac()): string {
  if (mac) return keys;
  return keys.replace("⌘", "Ctrl+").replace("⌥", "Alt+").replace("⇧", "Shift+").replace("⌃", "Ctrl+");
}

type KeyLike = Pick<KeyboardEvent, "key" | "code" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey">;

/// The global shortcut a key press means, or null. View-level shortcuts
/// (send, stop, quick open) are never matched here.
export function matchShortcut(e: KeyLike): { id: ShortcutId; pane?: string } | null {
  if (!(e.metaKey || e.ctrlKey)) return null;
  if (e.shiftKey && !e.altKey && (e.code === "BracketRight" || e.code === "BracketLeft")) {
    return { id: e.code === "BracketRight" ? "nextChat" : "prevChat" };
  }
  if (e.altKey && !e.shiftKey && /^Digit[0-9]$/.test(e.code)) {
    const pane = PANES[PANE_DIGITS.indexOf(e.code.slice(5))];
    return pane ? { id: "pane", pane: pane.id } : null;
  }
  if (e.altKey) return null;
  // ⌘/ (⌘? on layouts where / needs Shift).
  if (e.key === "/" || e.key === "?" || e.code === "Slash") return { id: "shortcuts" };
  const k = e.key.toLowerCase();
  if (k === "l" && !e.shiftKey) return { id: "composer" };
  if (k === "k") return { id: "palette" };
  if (k === "b") return { id: "toggleSidebar" };
  if (k === "j") return { id: "toggleTools" };
  if (e.key === ",") return { id: "settings" };
  if (e.key === "0") return { id: "zoomReset" };
  if (e.key === "+" || e.key === "=" || e.key === "NumpadAdd") return { id: "zoomIn" };
  if (e.key === "-" || e.key === "NumpadSubtract") return { id: "zoomOut" };
  return null;
}

/// Whether Esc should stop a streaming turn: a turn is running, nothing else
/// claimed the key (a dialog, a menu), and focus isn't in a text field that
/// has text in it (Esc there must never cost the draft).
export function escStopsTurn(p: {
  busy: boolean;
  defaultPrevented: boolean;
  modalOpen: boolean;
  menuOpen: boolean;
  target: EventTarget | null;
}): boolean {
  if (!p.busy || p.defaultPrevented || p.modalOpen || p.menuOpen) return false;
  const el = p.target as HTMLElement | null;
  if (el && typeof el === "object" && "tagName" in el) {
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
      return ((el as HTMLInputElement | HTMLTextAreaElement).value ?? "").length === 0;
    }
    if (el.isContentEditable) return (el.textContent ?? "").length === 0;
  }
  return true;
}
