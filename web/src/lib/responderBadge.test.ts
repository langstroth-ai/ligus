import { describe, it, expect } from "vitest";
import { responderBadge } from "./responderBadge";

describe("responderBadge", () => {
  it("labels this device's own @ligus turn by place, never by the default name 'You'", () => {
    const b = responderBadge("You", "this");
    expect(b?.label).toBe("This device");
    expect(b?.label.toLowerCase()).not.toBe("you");
  });
  it("another of your devices", () => {
    expect(responderBadge("You", "mine")?.label).toBe("Your other device");
  });
  it("someone else's device names its owner", () => {
    expect(responderBadge("Ada", null)).toEqual({ label: "Ada's device", title: "Answered by Ada's @ligus" });
    expect(responderBadge("", null)).toBeNull();
  });
});
