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
      ["ask", "Agents asked you 2 questions", "Relay backoff"],
      ["vote", "Your vote: Retry cap", "Relay backoff"],
      ["apply", "Ready to apply: Cold-day floor", "Relay backoff"],
      ["failed", "The last run failed", "Untitled chat"],
      ["sandbox", "This workspace's sandbox grants changed", undefined],
      ["trust", "This workspace wants to run 2 commands", undefined],
      ["device", "serve box says it's your device", undefined],
      ["invite", "Invite to join Bees", undefined],
    ]);
    // Each question counts, everything else once.
    expect(needsYouCount(items)).toBe(10);
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
