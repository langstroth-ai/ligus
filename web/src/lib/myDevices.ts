import type { DevicePairingDto, MyDeviceDto } from "@/lib/ipc";

/// The boxes that report a device as paired, most recently seen first.
export function pairingsOf(deviceId: string, pairings: DevicePairingDto[]): DevicePairingDto[] {
  return pairings
    .filter((p) => p.deviceId === deviceId)
    .sort((a, b) => (b.lastSeen ?? 0) - (a.lastSeen ?? 0));
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
export function ago(unixSeconds: number, nowMs: number = Date.now()): string {
  const s = Math.max(0, Math.floor(nowMs / 1000) - unixSeconds);
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86_400) return `${Math.round(s / 3600)} h ago`;
  const d = Math.round(s / 86_400);
  return `${d} day${d === 1 ? "" : "s"} ago`;
}

/// The name to show: the desktop's own, else the name a box knows the
/// phone by (another desktop added it), else the id fallback.
export function deviceName(d: MyDeviceDto, pairings: DevicePairingDto[]): string {
  if (d.named) return d.label;
  const p = pairingsOf(d.deviceId, pairings).find((x) => x.name.trim() !== "");
  return p ? p.name : d.label;
}

/// "Paired with home-server · seen 2 min ago" (none when no box reports it).
export function pairingLine(d: MyDeviceDto, pairings: DevicePairingDto[], nowMs: number = Date.now()): string | null {
  const ps = pairingsOf(d.deviceId, pairings);
  if (ps.length === 0) return null;
  const names = ps.map((p) => p.hostLabel);
  const latest = ps[0].lastSeen;
  return `Paired with ${names.join(", ")}${latest != null ? ` · seen ${ago(latest, nowMs)}` : ""}`;
}

/// The revoke confirmation, naming what happens to this device plainly.
export function revokeConfirmText(d: MyDeviceDto, pairings: DevicePairingDto[], nowMs: number = Date.now()): string {
  const name = deviceName(d, pairings);
  if (!d.phone) {
    return `Revoke ${name}? Every member stops accepting its events right away. To use it again, run \`ligus serve enroll --new-device\` on it and approve the new code.`;
  }
  const ps = pairingsOf(d.deviceId, pairings);
  const recent = ps.find((p) => p.lastSeen != null && nowMs / 1000 - p.lastSeen < 86_400);
  const which = recent
    ? `This is the iPhone paired with ${recent.hostLabel} right now (seen ${ago(recent.lastSeen!, nowMs)}). `
    : ps.length > 0
      ? `This iPhone is paired with ${ps.map((p) => p.hostLabel).join(", ")}. `
      : "";
  return (
    `Revoke ${name}? ${which}It will be signed out of this workspace and can't be added back with its current key. ` +
    `Anything it signed stays in the history. To use that phone again, choose Reset this phone's key in the app, then add it again.`
  );
}
