import { describe, it, expect } from "vitest";
import { COMMUNITY_RELAY_URL, relayUrlFor } from "./communityRelay";

describe("community relay preset", () => {
  it("is the https origin of relay.langstroth.ai", () => {
    expect(COMMUNITY_RELAY_URL).toBe("https://relay.langstroth.ai");
  });

  it("resolves each picker choice, with no silent fallback", () => {
    expect(relayUrlFor("community", "https://ignored.example")).toBe(COMMUNITY_RELAY_URL);
    expect(relayUrlFor("own", "  https://relay.acme.example  ")).toBe("https://relay.acme.example");
    expect(relayUrlFor("own", "")).toBe("");
    expect(relayUrlFor("none", "https://relay.acme.example")).toBe("");
  });
});
