import { describe, it, expect, beforeEach } from "vitest";
import { needsOnboarding, shouldAutoTour, clearLegacyFirstRunFlags } from "./firstRun";

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

  it("clears the 1.x flags", () => {
    window.localStorage.setItem("ligus.onboarded", "1");
    window.localStorage.setItem("ligus.tourSeen", "1");
    window.localStorage.setItem("ligus.theme", "harbor");
    clearLegacyFirstRunFlags();
    expect(window.localStorage.getItem("ligus.onboarded")).toBeNull();
    expect(window.localStorage.getItem("ligus.tourSeen")).toBeNull();
    expect(window.localStorage.getItem("ligus.theme")).toBe("harbor");
  });
});
