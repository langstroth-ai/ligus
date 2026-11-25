import { describe, it, expect } from "vitest";
import { isTerminalHostLabel, likelyMeReason, nameKey } from "./likelyMe";

const member = (displayName: string, extra: Partial<{ kind: string; isSelf: boolean; isFounder: boolean }> = {}) => ({
  displayName,
  kind: "human",
  isSelf: false,
  isFounder: false,
  ...extra,
});
const ltHost = (ownerName: string, label = "lt on Michaels-Air-3") => ({ label, ownerName, mine: false });

describe("likelyMeReason", () => {
  it("flags the owner's case: a shorter name with an lt host of its own", () => {
    expect(likelyMeReason(member("zaz"), "zazulam", [ltHost("zaz")])).toBe("name-and-terminal");
  });

  it("flags the same display name, ignoring case, spaces and punctuation", () => {
    expect(likelyMeReason(member("Mike Z."), "mike z", [])).toBe("same-name");
  });

  it("needs a terminal host of that member for a prefix match", () => {
    expect(likelyMeReason(member("zaz"), "zazulam", [])).toBeNull();
    expect(likelyMeReason(member("zaz"), "zazulam", [ltHost("someone")])).toBeNull();
    expect(likelyMeReason(member("zaz"), "zazulam", [ltHost("zaz", "build-box")])).toBeNull();
    expect(likelyMeReason(member("zaz"), "zazulam", [{ ...ltHost("zaz"), mine: true }])).toBeNull();
  });

  it("ignores short prefixes and different names", () => {
    expect(likelyMeReason(member("Al"), "Alex", [ltHost("Al")])).toBeNull();
    expect(likelyMeReason(member("Taylor"), "zazulam", [ltHost("Taylor")])).toBeNull();
  });

  it("never flags you, the founder, a box or an agent", () => {
    expect(likelyMeReason(member("zazulam", { isSelf: true }), "zazulam", [])).toBeNull();
    expect(likelyMeReason(member("zazulam", { isFounder: true }), "zazulam", [])).toBeNull();
    expect(likelyMeReason(member("zazulam", { kind: "service" }), "zazulam", [])).toBeNull();
    expect(likelyMeReason(member("zazulam", { kind: "agent" }), "zazulam", [])).toBeNull();
  });

  it("knows lt and ligus host labels", () => {
    expect(isTerminalHostLabel("lt on Michaels-Air-3")).toBe(true);
    expect(isTerminalHostLabel("ligus on build-box")).toBe(true);
    expect(isTerminalHostLabel("Lithium")).toBe(false);
    expect(nameKey("  Zaz-ulam ")).toBe("zazulam");
  });
});
