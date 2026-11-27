import { describe, it, expect, beforeEach } from "vitest";
import { needsOnboarding, shouldAutoTour, firstRunNext, firstRunDot, firstWorkspaceName, type FirstRunStep, type FirstRunEvent } from "./firstRun";

const settings = (onboarded: boolean, displayName: string) => ({ onboarded, displayName, tourSeen: false });

// The first-run gate follows Ligus's own data dir (settings.json, via
// getAppSettings), never the webview's localStorage, which survives a deleted
// data dir.
describe("first-run gate", () => {
  beforeEach(() => window.localStorage.clear());

  it("a fresh backend shows onboarding even with stale localStorage flags", () => {
    window.localStorage.setItem("ligus.onboarded", "1");
    window.localStorage.setItem("ligus.tourSeen", "1");
    expect(needsOnboarding(settings(false, "You"))).toBe(true);
  });

  it("a leftover display name does not skip onboarding", () => {
    expect(needsOnboarding(settings(false, "Ana"))).toBe(true);
  });

  it("completed onboarding hides it", () => {
    expect(needsOnboarding(settings(true, "Ana"))).toBe(false);
  });

  it("waits for settings to load", () => {
    expect(needsOnboarding(undefined)).toBe(false);
  });

  it("the tour auto-runs once, after onboarding, per data dir", () => {
    window.localStorage.setItem("ligus.tourSeen", "1");
    expect(shouldAutoTour({ onboarded: true, tourSeen: false })).toBe(true);
    expect(shouldAutoTour({ onboarded: true, tourSeen: true })).toBe(false);
    expect(shouldAutoTour({ onboarded: false, tourSeen: false })).toBe(false);
    expect(shouldAutoTour(undefined)).toBe(false);
  });
});

describe("first-run flow", () => {
  const EVENTS: FirstRunEvent[] = ["chooseFirst", "chooseLink", "back", "named", "secured", "askFinishLater", "keepGoing", "finishLater"];
  const run = (events: FirstRunEvent[], from: FirstRunStep = "fork") => events.reduce(firstRunNext, from);

  it("asks the question first, and each answer has its own way", () => {
    expect(run(["chooseFirst"])).toBe("you");
    expect(run(["chooseLink"])).toBe("link");
    expect(run(["chooseLink", "back"])).toBe("fork");
    expect(run(["chooseFirst", "back"])).toBe("fork");
  });

  it("a first device goes You, Secure, then the agent", () => {
    expect(run(["chooseFirst", "named"])).toBe("secure");
    expect(run(["chooseFirst", "named", "secured"])).toBe("agent");
  });

  it("the link screen never reaches a name, Secure or the agent", () => {
    // Every event from the link screen: it stays, or goes back to the question.
    for (const e of EVENTS) expect(["link", "fork"]).toContain(firstRunNext("link", e));
    // And no sequence of events from it reaches a first-device screen
    // without choosing "Yes, start here" again on the question.
    const reach = new Set<FirstRunStep>(["link"]);
    for (let i = 0; i < 4; i++)
      for (const s of [...reach]) for (const e of EVENTS.filter((x) => x !== "chooseFirst")) reach.add(firstRunNext(s, e));
    expect([...reach].sort()).toEqual(["fork", "link"]);
  });

  it("Secure has no way out but Finish later, which asks first", () => {
    for (const e of EVENTS) expect(["secure", "agent", "finishLater"]).toContain(firstRunNext("secure", e));
    expect(firstRunNext("secure", "back")).toBe("secure");
    expect(firstRunNext("secure", "finishLater")).toBe("secure");
    expect(run(["askFinishLater"], "secure")).toBe("finishLater");
    expect(run(["askFinishLater", "keepGoing"], "secure")).toBe("secure");
    expect(run(["askFinishLater", "finishLater"], "secure")).toBe("agent");
  });

  it("lights one dot per first-device screen", () => {
    expect(firstRunDot("fork")).toBe(0);
    expect(firstRunDot("link")).toBe(0);
    expect(firstRunDot("you")).toBe(1);
    expect(firstRunDot("secure")).toBe(2);
    expect(firstRunDot("finishLater")).toBe(2);
    expect(firstRunDot("agent")).toBe(3);
  });

  it("names the first workspace for the person", () => {
    expect(firstWorkspaceName("Ana Souza")).toBe("Ana's workspace");
    expect(firstWorkspaceName("  ")).toBe("My workspace");
  });
});
