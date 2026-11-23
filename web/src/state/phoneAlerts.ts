import { useSyncExternalStore } from "react";
import { onPhoneEnrollTripwire, type PhoneEnrollTripwireDto } from "@/lib/ipc";

// Add-a-phone tripwire alerts (another phone answered the code you showed),
// kept app-wide so the alert dialog and the Inbox read the same list. An
// alert stays until the person says the phone is theirs or revokes it.

let alerts: PhoneEnrollTripwireDto[] = [];
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

export const phoneAlerts = {
  add(a: PhoneEnrollTripwireDto) {
    if (alerts.some((x) => x.rid === a.rid && x.other.deviceId === a.other.deviceId)) return;
    alerts = [...alerts, a];
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
  snapshot(): readonly PhoneEnrollTripwireDto[] {
    return alerts;
  },
  /// Tests only.
  _reset() {
    alerts = [];
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

export function usePhoneAlerts(): readonly PhoneEnrollTripwireDto[] {
  return useSyncExternalStore(phoneAlerts.subscribe, phoneAlerts.snapshot, phoneAlerts.snapshot);
}
