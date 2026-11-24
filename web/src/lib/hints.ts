// One-time hints: a short line a viewer can dismiss once and not see again on
// this device. Stored per device like density (lib/density.ts); storage can be
// missing or blocked (private window, cleared site data), so every read and
// write is wrapped and a failure only means the hint shows again next time.

const PREFIX = "ligus.hint.dismissed.";

export function isHintDismissed(id: string): boolean {
  try {
    return localStorage.getItem(PREFIX + id) === "1";
  } catch {
    return false;
  }
}

export function dismissHint(id: string) {
  try {
    localStorage.setItem(PREFIX + id, "1");
  } catch {
    // Not remembered; it hides for this session only.
  }
}
