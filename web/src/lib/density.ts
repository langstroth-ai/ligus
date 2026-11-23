// Density: how much space rows, cards and the transcript take. Comfortable is
// the default; Compact and Spacious are opt-in. It is a CSS switch only
// (`data-density` on <html>, read by the variables in styles.css) and is
// separate from the UI zoom (⌘±). Stored per device.

export type Density = "compact" | "comfortable" | "spacious";

export const DENSITIES: { value: Density; label: string }[] = [
  { value: "compact", label: "Compact" },
  { value: "comfortable", label: "Comfortable" },
  { value: "spacious", label: "Spacious" },
];

const KEY = "ligus.density";
export const DEFAULT_DENSITY: Density = "comfortable";

export function loadDensity(): Density {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "compact" || v === "comfortable" || v === "spacious") return v;
  } catch {
    // Storage can be unavailable; the default applies.
  }
  return DEFAULT_DENSITY;
}

export function saveDensity(d: Density) {
  try {
    localStorage.setItem(KEY, d);
  } catch {
    // Not persisted, but still applied for this session.
  }
}

export function applyDensity(d: Density) {
  document.documentElement.dataset.density = d;
}
