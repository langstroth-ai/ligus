import { describe, it, expect } from "vitest";
import { contactsNote } from "./FriendsView";
import { trustNote } from "@/lib/people";

describe("contactsNote", () => {
  it("explains the link fallback when the relay has no contacts", () => {
    expect(contactsNote("unsupported")).toMatch(/links/);
    expect(contactsNote("noRelay")).toMatch(/links/);
    expect(contactsNote("on")).toBeNull();
  });
});

describe("trustNote", () => {
  it("never vouches for a name alone", () => {
    // A co-member's name on an invite whose code doesn't match theirs is a
    // warning, not a match (impersonation by display name).
    expect(trustNote("mismatch", "Ada")).toMatch(/doesn't match/);
    expect(trustNote("known", "Ada")).toMatch(/matches/);
    // Named and fingerprinted but not signed by their device: only a claim.
    expect(trustNote("claimed", "Ada")).toMatch(/^Says it's from Ada/);
    expect(trustNote("claimed", "Ada")).not.toMatch(/matches/);
    expect(trustNote("unknown", null)).toMatch(/Compare their safety code/);
    expect(trustNote("none", null)).toMatch(/nothing confirms/);
  });
});
