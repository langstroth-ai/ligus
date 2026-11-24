import { afterEach, describe, expect, it, vi } from "vitest";
import { dismissHint, isHintDismissed } from "./hints";

describe("one-time hints", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("remembers a dismissal per id", () => {
    expect(isHintDismissed("a")).toBe(false);
    dismissHint("a");
    expect(isHintDismissed("a")).toBe(true);
    expect(isHintDismissed("b")).toBe(false);
  });

  it("treats blocked storage as not dismissed and never throws", () => {
    const get = vi.spyOn(localStorage, "getItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    expect(() => dismissHint("c")).not.toThrow();
    expect(isHintDismissed("c")).toBe(false);
    expect(get).toHaveBeenCalled();
  });
});
