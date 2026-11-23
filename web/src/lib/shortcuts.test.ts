import { describe, it, expect } from "vitest";
import { SHORTCUTS, matchShortcut, escStopsTurn, keyLabel, paneKey } from "./shortcuts";
import { PANES } from "./panes";

const key = (over: Partial<KeyboardEvent>) =>
  ({ key: "", code: "", metaKey: true, ctrlKey: false, shiftKey: false, altKey: false, ...over }) as KeyboardEvent;

describe("matchShortcut", () => {
  it("matches the global bindings", () => {
    expect(matchShortcut(key({ key: "k", code: "KeyK" }))).toEqual({ id: "palette" });
    expect(matchShortcut(key({ key: "/", code: "Slash" }))).toEqual({ id: "shortcuts" });
    expect(matchShortcut(key({ key: "?", code: "Slash", shiftKey: true }))).toEqual({ id: "shortcuts" });
    expect(matchShortcut(key({ key: ",", code: "Comma" }))).toEqual({ id: "settings" });
    expect(matchShortcut(key({ key: "l", code: "KeyL" }))).toEqual({ id: "composer" });
    expect(matchShortcut(key({ key: "b", code: "KeyB" }))).toEqual({ id: "toggleSidebar" });
    expect(matchShortcut(key({ key: "j", code: "KeyJ" }))).toEqual({ id: "toggleTools" });
    expect(matchShortcut(key({ key: "}", code: "BracketRight", shiftKey: true }))).toEqual({ id: "nextChat" });
    expect(matchShortcut(key({ key: "{", code: "BracketLeft", shiftKey: true }))).toEqual({ id: "prevChat" });
    expect(matchShortcut(key({ key: "0", code: "Digit0" }))).toEqual({ id: "zoomReset" });
    expect(matchShortcut(key({ key: "=", code: "Equal" }))).toEqual({ id: "zoomIn" });
    expect(matchShortcut(key({ key: "-", code: "Minus" }))).toEqual({ id: "zoomOut" });
  });

  it("works with Ctrl as well as ⌘", () => {
    expect(matchShortcut(key({ key: "k", metaKey: false, ctrlKey: true }))).toEqual({ id: "palette" });
  });

  it("ignores keys without ⌘ or Ctrl", () => {
    expect(matchShortcut(key({ key: "k", metaKey: false }))).toBeNull();
    expect(matchShortcut(key({ key: "Escape", metaKey: false }))).toBeNull();
  });

  it("opens panes with ⌘⌥1…9 and the tenth, Activity, with ⌘⌥0", () => {
    expect(matchShortcut(key({ code: "Digit1", key: "¡", altKey: true }))).toEqual({ id: "pane", pane: PANES[0].id });
    expect(matchShortcut(key({ code: "Digit0", key: "º", altKey: true }))).toEqual({ id: "pane", pane: "activity" });
    expect(PANES[9].id).toBe("activity");
  });
});

describe("SHORTCUTS", () => {
  it("lists a key for every pane, Activity included", () => {
    for (const p of PANES) expect(paneKey(p.id)).toBeDefined();
    expect(paneKey("activity")).toBe("⌘⌥0");
  });

  it("has no two entries on the same keys", () => {
    const keys = SHORTCUTS.map((s) => s.keys);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("lists only keys the handler really matches (for the global ones)", () => {
    const viewLevel = new Set(["send", "newline", "stopTurn", "quickOpen"]);
    for (const s of SHORTCUTS.filter((s) => !viewLevel.has(s.id))) {
      const mods = { metaKey: s.keys.includes("⌘"), altKey: s.keys.includes("⌥"), shiftKey: s.keys.includes("⇧") };
      const last = s.keys.replace(/[⌘⌥⇧]/g, "");
      const ev = key({
        ...mods,
        key: last === "−" ? "-" : last,
        code: /^[0-9]$/.test(last) ? `Digit${last}` : last === "]" ? "BracketRight" : last === "[" ? "BracketLeft" : last === "/" ? "Slash" : "",
      });
      const hit = matchShortcut(ev);
      expect(hit?.id, s.keys).toBe(s.id);
      if (s.pane) expect(hit?.pane).toBe(s.pane);
    }
  });
});

describe("keyLabel", () => {
  it("keeps Mac symbols on a Mac and spells keys out elsewhere", () => {
    expect(keyLabel("⌘⌥0", true)).toBe("⌘⌥0");
    expect(keyLabel("⌘⌥0", false)).toBe("Ctrl+Alt+0");
    expect(keyLabel("⌘⇧]", false)).toBe("Ctrl+Shift+]");
    expect(keyLabel("Esc", false)).toBe("Esc");
  });
});

describe("escStopsTurn", () => {
  const base = { busy: true, defaultPrevented: false, modalOpen: false, menuOpen: false, target: null };
  const field = (tag: "textarea" | "input", value: string) => {
    const el = document.createElement(tag);
    el.value = value;
    return el;
  };

  it("stops a running turn from outside any field", () => {
    expect(escStopsTurn({ ...base, target: document.body })).toBe(true);
  });

  it("does nothing when no turn is running", () => {
    expect(escStopsTurn({ ...base, busy: false, target: document.body })).toBe(false);
  });

  it("stops from an empty message box", () => {
    expect(escStopsTurn({ ...base, target: field("textarea", "") })).toBe(true);
  });

  it("never acts from a field with text in it, so a draft is safe", () => {
    expect(escStopsTurn({ ...base, target: field("textarea", "half a thought") })).toBe(false);
    expect(escStopsTurn({ ...base, target: field("input", "x") })).toBe(false);
  });

  it("leaves Esc to a dialog or menu that is open, or that already took it", () => {
    expect(escStopsTurn({ ...base, modalOpen: true })).toBe(false);
    expect(escStopsTurn({ ...base, menuOpen: true })).toBe(false);
    expect(escStopsTurn({ ...base, defaultPrevented: true })).toBe(false);
  });
});
