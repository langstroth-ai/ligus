import { describe, expect, it } from "vitest";
import type { DevicePairingDto, MyDeviceDto } from "@/lib/ipc";
import { ago, deviceName, pairingLine, revokeConfirmText } from "@/lib/myDevices";

const NOW = 1_800_000_000_000;
const phone = (over: Partial<MyDeviceDto> = {}): MyDeviceDto => ({
  deviceId: "p1",
  label: "Ana's iPhone",
  fingerprint: "AAAA",
  thisDevice: false,
  revoked: false,
  lastSeen: null,
  phone: true,
  named: true,
  ...over,
});
const pairing = (over: Partial<DevicePairingDto> = {}): DevicePairingDto => ({
  deviceId: "p1",
  hostId: "h1",
  hostLabel: "home-server",
  name: "Ana's iPhone",
  lastSeen: NOW / 1000 - 120,
  reportedAt: NOW / 1000,
  ...over,
});

describe("My devices", () => {
  it("says which box a phone is paired with and when it was seen", () => {
    expect(pairingLine(phone(), [pairing()], NOW)).toBe("Paired with home-server · seen 2 min ago");
    expect(pairingLine(phone(), [pairing({ deviceId: "other" })], NOW)).toBeNull();
    expect(ago(NOW / 1000 - 3 * 86_400, NOW)).toBe("3 days ago");
  });

  it("names the phone in your hand in the revoke confirmation", () => {
    const text = revokeConfirmText(phone(), [pairing()], NOW);
    expect(text).toContain("This is the iPhone paired with home-server right now");
    expect(text).toContain("signed out of this workspace and can't be added back with its current key");
    expect(text).toContain("Reset this phone's key");
  });

  it("still says what revoking a phone does when no box reports it", () => {
    const text = revokeConfirmText(phone(), [], NOW);
    expect(text).not.toContain("right now");
    expect(text).toContain("can't be added back with its current key");
  });

  it("keeps the box wording for a box", () => {
    expect(revokeConfirmText(phone({ phone: false, label: "home-server" }), [], NOW)).toContain("ligus serve enroll --new-device");
  });

  it("uses a box's name for a phone this desktop has no name for", () => {
    expect(deviceName(phone({ named: false, label: "Device 1a2b3c4d" }), [pairing({ name: "Ana's iPhone" })])).toBe("Ana's iPhone");
    expect(deviceName(phone({ label: "Mine" }), [pairing({ name: "Theirs" })])).toBe("Mine");
  });
});
