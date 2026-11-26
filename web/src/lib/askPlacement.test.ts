import { describe, it, expect } from "vitest";
import type { AskDto } from "./ipc";
import { placeTurnAsks } from "./askPlacement";

const at = (hhmm: string) => Date.parse(`2026-11-26T${hhmm}:00Z`);

function ask(over: Partial<AskDto> = {}): AskDto {
  return {
    askId: "a1",
    chatId: "c1",
    turnId: "r1",
    agentName: "ligus",
    kind: "question",
    v: 2,
    question: "Which language?",
    options: ["Rust", "Speaking every language"],
    allowText: false,
    tally: [0, 1],
    expiresAtMs: at("10:17"),
    open: false,
    live: false,
    declined: false,
    answer: "Speaking every language",
    answeredBy: [{ memberId: "m1", name: "zazulam", atMs: at("10:09") }],
    olderAnswerUncounted: false,
    atMs: at("10:07"),
    ...over,
  } as AskDto;
}

describe("a turn's decision cards around its reply", () => {
  it("ask at 10:07, answered at 10:09, reply posted after: the answer reads before the reply", () => {
    const { before, after } = placeTurnAsks([ask()], { createdAt: "2026-11-26T10:10:00Z", isStreaming: false });
    expect(before.map((a) => a.askId)).toEqual(["a1"]);
    expect(after).toEqual([]);
  });

  it("a card still waiting stays after the reply, by the composer", () => {
    const live = ask({ open: true, live: true, answer: null, answeredBy: [] });
    expect(placeTurnAsks([live], { createdAt: "2026-11-26T10:10:00Z" }).after).toEqual([live]);
  });

  it("while the reply streams, its cards follow the live bubble", () => {
    expect(placeTurnAsks([ask()], { createdAt: "2026-11-26T10:07:00Z", isStreaming: true }).before).toEqual([]);
  });

  it("a reply stamped before the answer (an older build's turn start) keeps the card after it", () => {
    expect(placeTurnAsks([ask()], { createdAt: "2026-11-26T10:07:00Z" }).after.map((a) => a.askId)).toEqual(["a1"]);
  });
});
