import { describe, it, expect } from "vitest";
import { founderWaitTitle, isWaitingForFounder } from "./ChatView";

describe("waiting for the workspace's founder", () => {
  it("recognises the backend's refusal as a waiting state, not a failure", () => {
    expect(
      isWaitingForFounder(
        "unauthorized: waiting for the workspace's founder: their signed founding hasn't reached this device, so nothing is sent yet",
      ),
    ).toBe(true);
    expect(isWaitingForFounder("relay returned HTTP 500")).toBe(false);
  });
  it("names the founder when known", () => {
    expect(founderWaitTitle("Ada")).toBe("Waiting for Ada's founding to sync");
    expect(founderWaitTitle(null)).toBe("Waiting for the workspace's founder");
  });
});
