import { useSyncExternalStore } from "react";
import { onPhoneEnrollTripwire, type PhoneEnrollTripwireDto } from "@/lib/ipc";

// Add-a-phone tripwire alerts (another phone answered the code you showed),
// kept app-wide so the alert dialog and the Inbox read the same list. An
// alert stays until the person says the phone is theirs or revokes it.
// Closing the dialog only puts it aside: it waits in the Inbox, which can
// open it again.

let alerts: PhoneEnrollTripwireDto[] = [];
/// Whether the dialog is up. A new alert brings it up.
let open = false;
let snap = { alerts, open };
const listeners = new Set<() => void>();

function emit() {
  if (alerts.length === 0) open = false;
  snap = { alerts, open };
  for (const l of listeners) l();
}

export const phoneAlerts = {
  add(a: PhoneEnrollTripwireDto) {
    if (alerts.some((x) => x.rid === a.rid && x.other.deviceId === a.other.deviceId)) return;
    alerts = [...alerts, a];
    open = true;
    emit();
  },
  /// Put the dialog aside (the alert stays, in the Inbox).
  close() {
    open = false;
    emit();
  },
  /// Bring the dialog back (from the Inbox).
  show() {
    if (alerts.length === 0) return;
    open = true;
    emit();
  },
  /// Resolve one alert (it's mine, or revoked).
  resolve(rid: string, otherDeviceId: string) {
    alerts = alerts.filter((x) => !(x.rid === rid && x.other.deviceId === otherDeviceId));
    emit();
  },
  /// Every alert about a phone that was just revoked.
  resolveAdded(deviceId: string) {
    alerts = alerts.filter((x) => x.added.deviceId !== deviceId);
    emit();
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
  snapshot(): { alerts: readonly PhoneEnrollTripwireDto[]; open: boolean } {
    return snap;
  },
  /// Tests only.
  _reset() {
    alerts = [];
    open = false;
    emit();
  },
};

/// Listen for tripwire events (call once, at the app root).
export function listenPhoneAlerts(): () => void {
  const sub = onPhoneEnrollTripwire((a) => phoneAlerts.add(a));
  return () => {
    void sub.then((un) => un()).catch(() => {});
  };
}

/// The open alerts, oldest first.
export function usePhoneAlerts(): readonly PhoneEnrollTripwireDto[] {
  return useSyncExternalStore(phoneAlerts.subscribe, phoneAlerts.snapshot, phoneAlerts.snapshot).alerts;
}

/// Whether the alert dialog is up.
export function usePhoneAlertOpen(): boolean {
  return useSyncExternalStore(phoneAlerts.subscribe, phoneAlerts.snapshot, phoneAlerts.snapshot).open;
}
