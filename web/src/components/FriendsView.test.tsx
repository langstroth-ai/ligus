import { describe, it, expect } from "vitest";
import { contactsNote, inviteDeliveryNote } from "./FriendsView";
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

describe("inviteDeliveryNote", () => {
  const d = (state: string, extra = {}) =>
    ({ state, pending: ["waiting", "warning", "sending"].includes(state), delivered: state === "delivered", deliveredAt: null, expiresAt: 0, warning: null, ...extra }) as never;
  it("says a held invite will be delivered when their device is reachable", () => {
    expect(inviteDeliveryNote(d("waiting"), "Bea")).toMatch(/will be delivered when Bea's device is reachable/);
    expect(inviteDeliveryNote(d("waiting"), "Bea")).toMatch(/link yourself/);
  });
  it("warns when their keys changed and never claims delivery", () => {
    const n = inviteDeliveryNote(d("warning", { warning: "unverified recipient: ..." }), "Bea")!;
    expect(n).toMatch(/keys changed/);
    expect(n).toMatch(/wasn't sent/);
  });
  it("reports delivery and expiry, and nothing when nothing waits", () => {
    expect(inviteDeliveryNote(d("delivered"), "Bea")).toMatch(/got your invite/);
    expect(inviteDeliveryNote(d("expired"), "Bea")).toMatch(/7 days/);
    expect(inviteDeliveryNote(d("joined"), "Bea")).toBeNull();
    expect(inviteDeliveryNote(null, "Bea")).toBeNull();
  });
});
