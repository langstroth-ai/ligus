// The palette's recent items, newest first: the ids of what you ran from it.
// Per-device convenience only (localStorage); a missing or blocked store just
// means no recents.

const KEY = "ligus.palette.recent";
export const MAX_RECENTS = 12;

export function loadRecents(): string[] {
  try {
    const raw = window.localStorage.getItem(KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string").slice(0, MAX_RECENTS) : [];
  } catch {
    return [];
  }
}

export function noteRecent(id: string): string[] {
  const next = [id, ...loadRecents().filter((x) => x !== id)].slice(0, MAX_RECENTS);
  try {
    window.localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    /* recents are a convenience */
  }
  return next;
}
