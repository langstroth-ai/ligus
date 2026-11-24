import { describe, it, expect, vi } from "vitest";
import { SETUP_FLOWS, parseSetupCommand, setupAction, setupSlashItems } from "./setup";
import { OPEN_SETUP_EVENT } from "./nav";

describe("setup commands", () => {
  it("parses /setup in the composer", () => {
    expect(parseSetupCommand("/setup")).toBeNull();
    expect(parseSetupCommand("/setup agent")).toBe("agent");
    expect(parseSetupCommand("  /setup relay  ")).toBe("relay");
    expect(parseSetupCommand("/setup nope")).toBeUndefined();
    expect(parseSetupCommand("/setup agent now")).toBeUndefined();
    expect(parseSetupCommand("please /setup agent")).toBeUndefined();
  });

  it("completes /setup with every flow, and by name", () => {
    expect(setupSlashItems("setup").map((i) => i.flow)).toEqual(SETUP_FLOWS.map((f) => f.id));
    expect(setupSlashItems("mcp").map((i) => i.flow)).toEqual(["tool"]);
    expect(setupSlashItems("phone").map((i) => i.label)).toEqual(["Add a phone"]);
    expect(setupSlashItems("zzz")).toEqual([]);
  });

  it("an empty pane's action opens its flow", () => {
    const seen = vi.fn();
    const on = (e: Event) => seen((e as CustomEvent<string>).detail);
    window.addEventListener(OPEN_SETUP_EVENT, on);
    const a = setupAction("box");
    expect(a.label).toBe("Add a box");
    a.onClick();
    window.removeEventListener(OPEN_SETUP_EVENT, on);
    expect(seen).toHaveBeenCalledWith("box");
  });
});
