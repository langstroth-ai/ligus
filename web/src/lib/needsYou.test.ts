import { describe, expect, it } from "vitest";
import { needsYouCount, needsYouItems, needsYouLabel, reviewBadge } from "@/lib/needsYou";

const titles = new Map([
  ["c1", "Relay backoff"],
  ["c2", ""],
]);

describe("needsYouItems", () => {
  it("is empty when nothing waits on you", () => {
    expect(needsYouItems({})).toEqual([]);
    // A proposal you wrote or already voted on, and a lockdown, don't count.
    expect(
      needsYouItems({
        attention: [{ sessionId: "c1", openAsks: 0, paused: true, proposals: [{ id: "p1", title: "Mine", status: "open", needsMe: false }] }],
        chatTitles: titles,
      }),
    ).toEqual([]);
  });

  it("lists asks, votes, applies and failures, most urgent first", () => {
    const items = needsYouItems({
      attention: [
        {
          sessionId: "c1",
          openAsks: 2,
          paused: false,
          proposals: [
            { id: "p1", title: "Cold-day floor", status: "approved", needsMe: true },
            { id: "p2", title: "Retry cap", status: "open", needsMe: true },
          ],
        },
      ],
      live: new Map([
        ["c2", { live: false, failed: "model refused", finishedUnseen: false }],
        ["c1", { live: true, failed: null, finishedUnseen: false }],
      ]),
      chatTitles: titles,
      invites: [{ relay: "r", seq: 3, isDm: false, workspaceName: "Bees", founderName: "Maya" }],
      deviceRequests: [{ hostId: "h1", label: "serve box" }],
      trustWaiting: 2,
      sandboxChanged: true,
      phoneAlerts: [{ rid: "r1", otherDeviceId: "d9", addedLabel: "Ana's iPhone" }],
    });
    expect(items.map((i) => [i.kind, i.title, i.chatTitle])).toEqual([
      ["phone", "Another phone answered the code you showed for Ana's iPhone", undefined],
      ["sandbox", "This workspace's sandbox grants changed", undefined],
      ["trust", "This workspace wants to run 2 commands", undefined],
      ["ask", "Agents asked you 2 questions", "Relay backoff"],
      ["vote", "Your vote: Retry cap", "Relay backoff"],
      ["apply", "Ready to apply: Cold-day floor", "Relay backoff"],
      ["failed", "The last run failed", "Untitled chat"],
      ["device", "serve box says it's your device", undefined],
      ["invite", "Invite to join Bees", undefined],
    ]);
    // Each question counts, everything else once.
    expect(needsYouCount(items)).toBe(10);
  });

  it("keeps security alerts above any number of asks and votes", () => {
    // Another member can open many proposals and questions; a changed sandbox
    // grant or a trust request must still sit right under phone alerts.
    const proposals = Array.from({ length: 30 }, (_, i) => ({ id: `p${i}`, title: `P${i}`, status: "open", needsMe: true }));
    const items = needsYouItems({
      attention: [{ sessionId: "c1", openAsks: 9, paused: false, proposals }],
      chatTitles: titles,
      trustWaiting: 1,
      sandboxChanged: true,
      phoneAlerts: [{ rid: "r1", otherDeviceId: "d9", addedLabel: "" }],
    });
    expect(items.slice(0, 3).map((i) => i.kind)).toEqual(["phone", "sandbox", "trust"]);
  });

  it("names a direct-message invite by its sender", () => {
    const [i] = needsYouItems({ invites: [{ relay: "r", seq: 1, isDm: true, workspaceName: "", founderName: null }] });
    expect(i.title).toBe("Someone wants to message you");
  });

  it("says how many need you", () => {
    expect(needsYouLabel(1)).toBe("1 needs you");
    expect(needsYouLabel(3)).toBe("3 need you");
  });
});

describe("reviewBadge", () => {
  it("counts only this chat's proposals that wait on you", () => {
    const attention = [
      { sessionId: "c1", openAsks: 3, paused: false, proposals: [
        { id: "p1", title: "a", status: "open", needsMe: true },
        { id: "p2", title: "b", status: "open", needsMe: false },
        { id: "p3", title: "c", status: "approved", needsMe: true },
      ] },
      { sessionId: "c2", openAsks: 0, paused: false, proposals: [{ id: "p4", title: "d", status: "open", needsMe: false }] },
    ];
    expect(reviewBadge(attention, "c1")).toBe(2);
    expect(reviewBadge(attention, "c2")).toBeUndefined();
    expect(reviewBadge(attention, null)).toBeUndefined();
  });
});

describe("approval requests", () => {
  const a = (rid: string, kind: "box" | "computer" | "phone", label: string, createdAt = 1) => ({ workspaceId: "w1", rid, kind, label, createdAt });

  it("rank right after a phone alert, above the other security rows and asks", () => {
    const items = needsYouItems({
      sandboxChanged: true,
      trustWaiting: 1,
      attention: [{ sessionId: "c1", openAsks: 1, proposals: [] } as never],
      phoneAlerts: [{ rid: "p", otherDeviceId: "o", addedLabel: "iPhone" }],
      approvals: [a("r1", "box", "build-1")],
      approvalFacts: [{ key: "mismatch:r0", kind: "mismatch", label: "build-0", at: 1 }],
    });
    expect(items.map((i) => i.kind)).toEqual(["phone", "approval", "approvalFact", "sandbox", "trust", "ask"]);
    expect(items[1].title).toBe("build-1 wants to join");
    expect(items[2].title).toBe("Request from build-0 didn't match. Nothing was granted.");
  });

  it("names a device of mine, and a link that didn't finish", () => {
    const items = needsYouItems({
      approvals: [a("r1", "computer", "lt on mbp")],
      approvalFacts: [{ key: "didntFinish:r2", kind: "didntFinish", label: "build-2", at: 1 }],
    });
    expect(items.map((i) => i.title)).toEqual(["lt on mbp wants to be your device", "Linking build-2 didn't finish. Nothing was added."]);
  });

  it("collapse past three into one row that counts each request; facts don't count", () => {
    const four = [a("r1", "box", "b1", 1), a("r2", "box", "b2", 2), a("r3", "computer", "c3", 3), a("r4", "box", "b4", 4)];
    const items = needsYouItems({ approvals: four, approvalFacts: [{ key: "k", kind: "mismatch", label: "x", at: 1 }] });
    const rows = items.filter((i) => i.kind === "approval");
    expect(rows).toHaveLength(1);
    expect(rows[0].title).toBe("4 requests to approve");
    expect(rows[0].approvals).toHaveLength(4);
    expect(needsYouCount(items)).toBe(4);
    expect(needsYouItems({ approvals: four.slice(0, 3) }).filter((i) => i.kind === "approval")).toHaveLength(3);
  });
});

