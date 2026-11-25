// My devices: revoked devices kept out of the way. Hiding is a per-viewer
// display choice on this device only (localStorage, like lib/hints.ts): it
// never touches the revocation, which stays in the workspace log so every
// member keeps refusing that device. Storage can be missing or blocked; a
// failure only means the row shows again.

import type { DeviceRevocationDto, MyDeviceDto } from "@/lib/ipc";

const HIDDEN_KEY = "ligus.devices.hiddenRevoked";
const OPEN_KEY = "ligus.devices.revokedOpen";

export function loadHiddenRevoked(): Set<string> {
  try {
    const raw = localStorage.getItem(HIDDEN_KEY);
    const ids = raw ? (JSON.parse(raw) as unknown) : [];
    return new Set(Array.isArray(ids) ? ids.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function saveHiddenRevoked(ids: Set<string>) {
  try {
    localStorage.setItem(HIDDEN_KEY, JSON.stringify([...ids]));
  } catch {
    // Not remembered: it shows again next time.
  }
}

export function loadRevokedOpen(): boolean {
  try {
    return localStorage.getItem(OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveRevokedOpen(open: boolean) {
  try {
    localStorage.setItem(OPEN_KEY, open ? "1" : "0");
  } catch {
    // Collapsed again next time.
  }
}

/// A revoked device's cleanup that isn't finished on some box (a tunnel not
/// cut, a push token not removed): it stays in view with its retry.
export function unfinishedCleanup(deviceId: string, reports: DeviceRevocationDto[]): DeviceRevocationDto[] {
  return reports.filter((r) => r.deviceId === deviceId && !r.complete);
}

/// "tunnel not cut on home-server" — the first unfinished step, one line.
export function cleanupWarning(r: DeviceRevocationDto): string {
  const left = [
    r.tunnel.state !== "done" && r.tunnel.state !== "not_applicable" ? "tunnel not cut" : null,
    r.push.state !== "done" && r.push.state !== "not_applicable" ? "push not removed" : null,
    r.session.state !== "done" && r.session.state !== "not_applicable" ? "session not ended" : null,
  ].filter(Boolean);
  return `${left.length ? left.join(", ") : "cleanup unfinished"} on ${r.hostLabel}`;
}

/// When it was revoked, from the boxes' reports (a phone); none for a
/// computer, whose revocation no box reports.
export function revokedOn(deviceId: string, reports: DeviceRevocationDto[]): string | null {
  const at = reports.filter((r) => r.deviceId === deviceId).map((r) => r.revokedAt).filter((t) => t > 0);
  if (at.length === 0) return null;
  // Unix seconds: when the first box folded the revocation.
  return new Date(Math.min(...at) * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

export type DeviceGroups = {
  active: MyDeviceDto[];
  /// Revoked, cleanup unfinished somewhere: shown with the active ones.
  attention: MyDeviceDto[];
  /// Revoked and done, not hidden: the collapsed "Revoked (N)" group.
  revoked: MyDeviceDto[];
  /// Revoked and hidden here.
  hidden: MyDeviceDto[];
};

export function groupDevices(list: MyDeviceDto[], reports: DeviceRevocationDto[], hidden: Set<string>): DeviceGroups {
  const g: DeviceGroups = { active: [], attention: [], revoked: [], hidden: [] };
  for (const d of list) {
    if (!d.revoked) g.active.push(d);
    else if (unfinishedCleanup(d.deviceId, reports).length > 0) g.attention.push(d);
    else if (hidden.has(d.deviceId)) g.hidden.push(d);
    else g.revoked.push(d);
  }
  return g;
}
