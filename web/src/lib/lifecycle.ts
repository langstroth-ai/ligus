// Member and device lifecycles (workspace model v3, 10.1 and 10.4): who
// asked to leave, which devices and hosts went idle, and the entries a
// person removed from view. Removing from view is a display choice on this
// device only (localStorage, like lib/revokedDevices.ts): it never touches
// the workspace, where the member is still listed and the device still
// revoked. Storage can be missing or blocked; a failure only means the row
// shows again.

import type { DevicePairingDto, DeviceState, MyDeviceDto } from "@/lib/ipc";

/// Days without a signature or a beat after which a device or host is Idle.
export const IDLE_AFTER_DAYS = 30;

const HIDDEN_LEFT_KEY = "ligus.people.hiddenLeft";

export function loadHiddenLeft(): Set<string> {
  try {
    const raw = localStorage.getItem(HIDDEN_LEFT_KEY);
    const ids = raw ? (JSON.parse(raw) as unknown) : [];
    return new Set(Array.isArray(ids) ? ids.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function saveHiddenLeft(ids: Set<string>) {
  try {
    localStorage.setItem(HIDDEN_LEFT_KEY, JSON.stringify([...ids]));
  } catch {
    // Not remembered: it shows again next time.
  }
}

/// Whether a host last seen at `iso` (RFC 3339) is idle now.
export function isIdleSince(iso: string | null | undefined, nowMs: number = Date.now()): boolean {
  if (!iso) return false;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return false;
  return nowMs - then >= IDLE_AFTER_DAYS * 86_400_000;
}

/// A device's state on this page: the backend's, except that a phone a box
/// saw within the idle window is Active (its pairing is its beat).
export function deviceState(d: MyDeviceDto, pairings: DevicePairingDto[], nowMs: number = Date.now()): DeviceState {
  const s = d.state ?? (d.revoked ? "revoked" : d.neverWritten ? "pending" : "active");
  if (s !== "idle") return s;
  const seen = pairings.filter((p) => p.deviceId === d.deviceId && p.lastSeen != null).map((p) => p.lastSeen!);
  return seen.some((t) => nowMs / 1000 - t < IDLE_AFTER_DAYS * 86_400) ? "active" : "idle";
}

/// "asked to leave 24 Nov" for a member who left on their own.
export function leftLine(leftAt: string): string {
  const t = Date.parse(leftAt);
  if (Number.isNaN(t)) return "asked to leave";
  return `asked to leave ${new Date(t).toLocaleDateString(undefined, { day: "numeric", month: "short" })}`;
}
