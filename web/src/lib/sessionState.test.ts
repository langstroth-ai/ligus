import { describe, it, expect, beforeEach } from "vitest";
import { sessionState, sessionStates, SESSION_STATE_META, type ChatAttention } from "./sessionState";
import { liveTurns, LIVE_STALE_MS } from "@/state/liveTurns";
import type { QueuedWorkDto } from "@/bindings/QueuedWorkDto";

describe("sessionState", () => {
  it("is idle with nothing going on", () => {
    expect(sessionState({})).toBe("idle");
    expect(sessionState({ openAsks: 0, proposalsNeedingMe: 0, queued: 0 })).toBe("idle");
  });

  it("maps each signal to its state", () => {
    expect(sessionState({ live: true })).toBe("working");
    expect(sessionState({ openAsks: 1 })).toBe("needsYou");
    expect(sessionState({ proposalsNeedingMe: 2 })).toBe("needsYou");
    expect(sessionState({ queued: 1 })).toBe("queued");
    expect(sessionState({ finishedUnseen: true })).toBe("done");
    expect(sessionState({ failed: true })).toBe("failed");
    expect(sessionState({ paused: true })).toBe("paused");
  });

  it("puts what waits on you above everything else", () => {
    expect(sessionState({ live: true, openAsks: 1 })).toBe("needsYou");
    expect(sessionState({ failed: true, proposalsNeedingMe: 1 })).toBe("needsYou");
    expect(sessionState({ paused: true, openAsks: 1 })).toBe("needsYou");
  });

  it("ranks failed over paused over working over queued over done", () => {
    expect(sessionState({ failed: true, paused: true, live: true })).toBe("failed");
    expect(sessionState({ paused: true, live: true })).toBe("paused");
    expect(sessionState({ live: true, queued: 1 })).toBe("working");
    expect(sessionState({ queued: 1, finishedUnseen: true })).toBe("queued");
  });

  it("gives every non-idle state a word and a description", () => {
    for (const meta of Object.values(SESSION_STATE_META)) {
      expect(meta.word.length).toBeGreaterThan(0);
      expect(meta.describe.length).toBeGreaterThan(meta.word.length);
    }
  });
});

describe("sessionStates", () => {
  const queued = (sessionId: string): QueuedWorkDto => ({
    sessionId,
    chatTitle: "",
    agentId: "a",
    agentName: "builder",
    hostId: "h",
    hostLabel: "serve-box",
    hostStatus: "offline",
  });
  const attention = (sessionId: string, over: Partial<ChatAttention> = {}): ChatAttention => ({
    sessionId,
    openAsks: 0,
    proposals: [],
    paused: false,
    ...over,
  });

  it("combines the sources and leaves idle chats out", () => {
    const states = sessionStates(["a", "b", "c", "d", "e", "idle"], {
      attention: [
        attention("a", { openAsks: 1 }),
        attention("b", { proposals: [{ id: "p", title: "x", status: "open", needsMe: true }] }),
        // A proposal that doesn't wait on me doesn't make the chat need me.
        attention("e", { proposals: [{ id: "q", title: "y", status: "open", needsMe: false }] }),
      ],
      queued: [queued("c"), queued("c")],
      live: new Map([["d", { live: true, failed: null, finishedUnseen: false }]]),
    });
    expect(Object.fromEntries(states)).toEqual({ a: "needsYou", b: "needsYou", c: "queued", d: "working" });
  });

  it("ignores data for chats that aren't listed", () => {
    const states = sessionStates(["a"], { attention: [attention("zzz", { openAsks: 3 })] });
    expect(states.size).toBe(0);
  });
});

describe("liveTurns", () => {
  let t = 0;
  beforeEach(() => {
    t = 1_000;
    liveTurns._reset(() => t);
  });
  const stream = (sessionId: string, messageId: string, phase: string, text = "") =>
    liveTurns.onStream({ sessionId, messageId, phase, text });

  it("shows a chat working while it streams, then done when it finishes off screen", () => {
    stream("s1", "m1", "delta", "he");
    expect(liveTurns.snapshot().get("s1")).toEqual({ live: true, failed: null, finishedUnseen: false });
    stream("s1", "m1", "completed");
    expect(liveTurns.snapshot().get("s1")).toEqual({ live: false, failed: null, finishedUnseen: true });
    liveTurns.setViewing("s1");
    expect(liveTurns.snapshot().has("s1")).toBe(false);
  });

  it("doesn't call a turn you watched finish news", () => {
    liveTurns.setViewing("s1");
    stream("s1", "m1", "delta");
    stream("s1", "m1", "completed");
    expect(liveTurns.snapshot().has("s1")).toBe(false);
  });

  it("stays working until the last parallel stream ends", () => {
    stream("s1", "m1", "delta");
    stream("s1", "m2", "delta");
    stream("s1", "m1", "completed");
    expect(liveTurns.snapshot().get("s1")?.live).toBe(true);
    stream("s1", "m2", "completed");
    expect(liveTurns.snapshot().get("s1")?.live).toBe(false);
  });

  it("remembers a failure until it is cleared or a new turn starts", () => {
    stream("s1", "m1", "delta");
    stream("s1", "m1", "error", "model went away");
    expect(liveTurns.snapshot().get("s1")).toEqual({ live: false, failed: "model went away", finishedUnseen: false });
    expect(liveTurns.failureOf("s1")).toBe("model went away");
    stream("s1", "m2", "delta");
    expect(liveTurns.failureOf("s1")).toBeNull();
    stream("s1", "m2", "error", "");
    expect(liveTurns.failureOf("s1")).toMatch(/failed|interrupted/i);
    liveTurns.clearFailure("s1");
    expect(liveTurns.snapshot().has("s1")).toBe(false);
  });

  it("treats a stop as neither a failure nor news", () => {
    stream("s1", "m1", "delta");
    liveTurns.stopped("s1");
    expect(liveTurns.snapshot().has("s1")).toBe(false);
  });

  it("covers the gap between sending and the first delta", () => {
    liveTurns.setViewing("s1");
    liveTurns.setSending("s1", true);
    expect(liveTurns.snapshot().get("s1")?.live).toBe(true);
    liveTurns.setSending("s1", false);
    expect(liveTurns.snapshot().has("s1")).toBe(false);
  });

  it("lets a turn that went silent stop reading as working", () => {
    stream("s1", "m1", "delta");
    t += LIVE_STALE_MS - 1;
    liveTurns.onActivity("s1"); // a tool call keeps it alive
    t += LIVE_STALE_MS - 1;
    liveTurns.refresh();
    expect(liveTurns.snapshot().get("s1")?.live).toBe(true);
    t += 2;
    liveTurns.refresh();
    expect(liveTurns.snapshot().has("s1")).toBe(false);
  });
});
