import { describe, it, expect } from "vitest";
import { cleanName, detectMention, filterMentions, splitYouMentions, type MentionCandidate } from "./mentions";

describe("detectMention", () => {
  it("detects a bare @ at the caret", () => {
    expect(detectMention("@", 1)).toEqual({ start: 0, query: "" });
  });

  it("captures the partial handle before the caret", () => {
    expect(detectMention("hey @sa", 7)).toEqual({ start: 4, query: "sa" });
  });

  it("triggers at the start of input", () => {
    expect(detectMention("@mara", 5)).toEqual({ start: 0, query: "mara" });
  });

  it("does NOT trigger inside an email (no leading boundary)", () => {
    expect(detectMention("email me at sam@x", 17)).toBeNull();
  });

  it("does NOT trigger once the token is closed by a space", () => {
    expect(detectMention("@sam ", 5)).toBeNull();
  });

  it("only considers the token immediately before the caret", () => {
    // caret is mid-string after "@bo"
    expect(detectMention("@al said hi @bo", 15)).toEqual({ start: 12, query: "bo" });
  });

  it("allows hyphens in handles", () => {
    expect(detectMention("@code-rev", 9)).toEqual({ start: 0, query: "code-rev" });
  });
});

describe("filterMentions", () => {
  const cands: MentionCandidate[] = [
    { handle: "primary", kind: "Primary runtime" },
    { handle: "all", kind: "Everyone" },
    { handle: "Scout", kind: "Agent" },
    { handle: "Sam", kind: "owner" },
  ];

  it("is a case-insensitive prefix match", () => {
    expect(filterMentions(cands, "s").map((c) => c.handle)).toEqual(["Scout", "Sam"]);
    expect(filterMentions(cands, "SC").map((c) => c.handle)).toEqual(["Scout"]);
  });

  it("returns everything for an empty query", () => {
    expect(filterMentions(cands, "")).toHaveLength(4);
  });

  it("does not match mid-string (prefix only)", () => {
    expect(filterMentions(cands, "out")).toEqual([]);
  });

  it("respects the limit", () => {
    const many = Array.from({ length: 20 }, (_, i) => ({ handle: `a${i}`, kind: "x" }));
    expect(filterMentions(many, "a", 5)).toHaveLength(5);
  });
});

describe("splitYouMentions", () => {
  it("splits around @you with the parser's boundaries", () => {
    expect(splitYouMentions("@you can you check?")).toEqual([null, " can you check?"]);
    expect(splitYouMentions("Done, @You.")).toEqual(["Done, ", null, "."]);
    expect(splitYouMentions("mail me@you.io or @young")).toEqual(["mail me@you.io or @young"]);
    expect(splitYouMentions("@you-know-who")).toEqual(["@you-know-who"]);
  });
});

describe("cleanName", () => {
  it("drops hidden characters and collapses whitespace", () => {
    expect(cleanName(" A‮na​  B ")).toBe("Ana B");
  });
});

describe("agent access in the @mention autocomplete", () => {
  it("an agent you can't use stays listed, greyed out with its reason, and can't be inserted", async () => {
    const { agentMentionCandidates, mentionSelectable } = await import("./mentions");
    const cands = agentMentionCandidates([
      { name: "scout", available: true },
      { name: "private", available: false, unavailableReason: "Only its owner can use this agent" },
      { name: "legacy" },
    ]);
    expect(cands.map((c) => c.handle)).toEqual(["scout", "private", "legacy"]);
    const priv = cands[1];
    expect(priv.disabledReason).toBe("Only its owner can use this agent");
    expect(priv.kind).toBe("Not available");
    expect(mentionSelectable(priv)).toBe(false);
    expect(mentionSelectable(cands[0])).toBe(true);
    expect(mentionSelectable(cands[2])).toBe(true);
    expect(filterMentions(cands, "pri")).toEqual([priv]);
  });
});

describe("the default agent's handle", () => {
  it("a roster agent named ligus or primary is never offered: @ligus is the default agent's", async () => {
    const { agentMentionCandidates } = await import("./mentions");
    const cands = agentMentionCandidates([
      { name: "Ligus" },
      { name: " primary " },
      { name: "L\u0456gus" },
      { name: "li\u200bgus" },
      { name: "scout" },
      { name: "ligusbot" },
      { name: "hive" },
    ]);
    // A custom agent deliberately named "hive" keeps its mention.
    expect(cands.map((c) => c.handle)).toEqual(["scout", "ligusbot", "hive"]);
  });
});
