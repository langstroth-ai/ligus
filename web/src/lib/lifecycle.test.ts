import { describe, it, expect, beforeEach } from "vitest";
import { deviceState, isIdleSince, leftLine, loadHiddenLeft, saveHiddenLeft, IDLE_AFTER_DAYS } from "@/lib/lifecycle";
import type { DevicePairingDto, MyDeviceDto } from "@/lib/ipc";

const now = Date.parse("2026-11-27T12:00:00Z");
const daysAgo = (d: number) => new Date(now - d * 86_400_000).toISOString();

const dev = (over: Partial<MyDeviceDto>): MyDeviceDto => ({
  deviceId: "p1",
  label: "Phone",
  fingerprint: "AAAA",
  thisDevice: false,
  revoked: false,
  lastSeen: null,
  phone: true,
  named: true,
  ...over,
});

const pairing = (lastSeenDays: number): DevicePairingDto =>
  ({ deviceId: "p1", hostId: "h", hostLabel: "box", name: "", lastSeen: Math.floor((now - lastSeenDays * 86_400_000) / 1000), reportedAt: 0 }) as DevicePairingDto;

describe("device and host lifecycles", () => {
  it("a host is idle after 30 days without a beat", () => {
    expect(isIdleSince(daysAgo(IDLE_AFTER_DAYS), now)).toBe(true);
    expect(isIdleSince(daysAgo(IDLE_AFTER_DAYS - 1), now)).toBe(false);
    expect(isIdleSince(null, now)).toBe(false);
    expect(isIdleSince("not a time", now)).toBe(false);
  });

  it("takes the backend's state, and a phone a box saw lately is active", () => {
    expect(deviceState(dev({ state: "idle" }), [], now)).toBe("idle");
    expect(deviceState(dev({ state: "idle" }), [pairing(2)], now)).toBe("active");
    expect(deviceState(dev({ state: "idle" }), [pairing(40)], now)).toBe("idle");
    expect(deviceState(dev({ state: "pending" }), [pairing(1)], now)).toBe("pending");
    // An older backend sends no state.
    expect(deviceState(dev({ revoked: true }), [], now)).toBe("revoked");
    expect(deviceState(dev({ neverWritten: true }), [], now)).toBe("pending");
    expect(deviceState(dev({}), [], now)).toBe("active");
  });

  it("names a leave by its day", () => {
    expect(leftLine("2026-11-24T10:00:00Z")).toMatch(/^asked to leave /);
    expect(leftLine("garbage")).toBe("asked to leave");
  });
});

describe("remove from view", () => {
  beforeEach(() => localStorage.clear());
  it("is remembered on this device only and survives bad storage", () => {
    expect(loadHiddenLeft().size).toBe(0);
    saveHiddenLeft(new Set(["m1"]));
    expect(loadHiddenLeft().has("m1")).toBe(true);
    localStorage.setItem("ligus.people.hiddenLeft", "{not json");
    expect(loadHiddenLeft().size).toBe(0);
  });
});
