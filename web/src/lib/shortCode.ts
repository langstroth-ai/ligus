// Short link codes (`K7Q-4MX`, ligus_core::link_code): six Crockford base32
// symbols. Typing is forgiving: case, spaces and a dash don't matter, and O
// reads as 0, I and L as 1. The desktop checks it again.

const SYMBOLS = /^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{6}$/;

/// The six symbols of a typed short code, or null when it isn't one.
export function parseShortCode(text: string): string | null {
  const t = text
    .trim()
    .replace(/[\s\-‐‑–]/g, "")
    .toUpperCase()
    .replace(/O/g, "0")
    .replace(/[IL]/g, "1");
  return SYMBOLS.test(t) ? t : null;
}

/// A relay URL as a person reads it: its host.
export function relayHost(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}
