import { describe, expect, it } from "vitest";
import { SETTINGS_TABS, parseSettingsTab } from "./SettingsView";

describe("parseSettingsTab", () => {
  it("resolves every tab id to itself (deep links never fall back to Account)", () => {
    for (const t of SETTINGS_TABS) expect(parseSettingsTab(t.id)).toBe(t.id);
  });
  it("keeps legacy names working", () => {
    expect(parseSettingsTab("team")).toBe("team-sync");
    expect(parseSettingsTab("dearth")).toBe("guards");
    expect(parseSettingsTab("nonsense")).toBe("account");
  });
});
