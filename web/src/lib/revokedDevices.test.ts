import { describe, it, expect, beforeEach } from "vitest";
import type { DeviceRevocationDto, MyDeviceDto } from "@/lib/ipc";
import {
  cleanupWarning,
  groupDevices,
  loadHiddenRevoked,
  loadRevokedOpen,
  saveHiddenRevoked,
  saveRevokedOpen,
} from "./revokedDevices";

const dev = (deviceId: string, revoked: boolean): MyDeviceDto => ({
  deviceId,
  label: `Device ${deviceId}`,
  fingerprint: "0C3B",
  thisDevice: false,
  revoked,
  lastSeen: null,
  phone: true,
  named: false,
});
const done = { state: "done" as const, at: 1, attempts: 1 };
const report = (deviceId: string, complete: boolean): DeviceRevocationDto => ({
  hostId: "h1",
  hostLabel: "home-server",
  reportedAt: 1,
  summary: "revoked",
  complete,
  deviceId,
  name: "",
  revokedAt: 1_700_000_000,
  tunnel: complete ? done : { state: "failed", at: 1, attempts: 2 },
  push: done,
  session: { state: "not_applicable", at: 1, attempts: 0 },
});

describe("groupDevices", () => {
  beforeEach(() => localStorage.clear());

  it("keeps active rows, lifts unfinished cleanups, folds the rest and hides what's hidden", () => {
    const g = groupDevices(
      [dev("a", false), dev("b", true), dev("c", true), dev("d", true)],
      [report("b", false), report("c", true)],
      new Set(["d", "b"]),
    );
    expect(g.active.map((d) => d.deviceId)).toEqual(["a"]);
    // Hidden doesn't hide an unfinished cleanup.
    expect(g.attention.map((d) => d.deviceId)).toEqual(["b"]);
    expect(g.revoked.map((d) => d.deviceId)).toEqual(["c"]);
    expect(g.hidden.map((d) => d.deviceId)).toEqual(["d"]);
  });

  it("says which step is left, on one line", () => {
    expect(cleanupWarning(report("b", false))).toBe("tunnel not cut on home-server");
  });

  it("remembers the group and the hidden ones per viewer, and survives blocked storage", () => {
    expect(loadRevokedOpen()).toBe(false);
    saveRevokedOpen(true);
    expect(loadRevokedOpen()).toBe(true);
    saveHiddenRevoked(new Set(["x"]));
    expect([...loadHiddenRevoked()]).toEqual(["x"]);
    localStorage.setItem("ligus.devices.hiddenRevoked", "{not json");
    expect(loadHiddenRevoked().size).toBe(0);
  });
});
