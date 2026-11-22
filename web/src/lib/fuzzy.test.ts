import { describe, it, expect } from "vitest";
import { fuzzyScore, fuzzyRank } from "./fuzzy";

describe("fuzzyScore", () => {
  it("matches an in-order subsequence and rejects anything else", () => {
    expect(fuzzyScore("opst", "Open settings")).not.toBeNull();
    expect(fuzzyScore("tsop", "Open settings")).toBeNull();
    expect(fuzzyScore("xyz", "Open settings")).toBeNull();
  });

  it("treats an empty query as a neutral match", () => {
    expect(fuzzyScore("", "anything")).toBe(0);
    expect(fuzzyScore("   ", "anything")).toBe(0);
  });

  it("is case-insensitive", () => {
    expect(fuzzyScore("OPEN", "open review")).not.toBeNull();
  });

  it("prefers a substring over a scattered match", () => {
    expect(fuzzyScore("rev", "Open Review")!).toBeGreaterThan(fuzzyScore("rev", "Run every view")!);
  });

  it("prefers word starts and a prefix", () => {
    expect(fuzzyScore("set", "Settings: Guards")!).toBeGreaterThan(fuzzyScore("set", "Reset the layout")!);
    expect(fuzzyScore("na", "New agent")!).toBeGreaterThan(fuzzyScore("na", "Open settings tab")!);
  });
});

describe("fuzzyRank", () => {
  const items = ["New chat", "Open settings", "Settings: Appearance", "Add a phone", "Open Activity"];

  it("orders by score and drops non-matches", () => {
    expect(fuzzyRank(items, "set", (s) => s)).toEqual(["Settings: Appearance", "Open settings"]);
    expect(fuzzyRank(items, "phone", (s) => s)).toEqual(["Add a phone"]);
  });

  it("keeps input order for an empty query", () => {
    expect(fuzzyRank(items, "", (s) => s)).toEqual(items);
  });

  it("lets a bonus lift an item, e.g. a recent one", () => {
    const ranked = fuzzyRank(items, "op", (s) => s, (s) => (s === "Open Activity" ? 50 : 0));
    expect(ranked[0]).toBe("Open Activity");
  });
});
