// The workspace's default agent — the one that answers untagged messages (in a
// solo workspace) and any message tagged @ligus or @primary. It is synthesized
// by the backend at dispatch time (no roster record), so the UI names it from
// these constants wherever it must appear: the turn head, the route pill, the
// mention list, and the roster/count.
//
//   PRIMARY_NAME   — display name + avatar initials ("Ligus")
//   PRIMARY_HANDLE — mentionable handle, rendered "@ligus"
//
// "@primary" stays a reserved router alias (a role, not a name); both resolve to
// this same default agent.
export const PRIMARY_NAME = "Ligus";
export const PRIMARY_HANDLE = "ligus";

// Whether a roster agent's name always means the default agent ("ligus" or
// "primary"). No current client creates such an agent; one that syncs in
// anyway (an older client, a hand-made event) is never offered as a mention
// or matched as the author of the default agent's replies, mirroring the
// backend (`ligus_core::agent::names_primary`). The Hive builds' "@hive" is a
// backend-only alias: the UI always shows and inserts @ligus.
export function namesPrimary(name: string): boolean {
  const n = reservedSkeleton(name);
  return n === reservedSkeleton(PRIMARY_HANDLE) || n === reservedSkeleton("primary");
}

// A rough twin of the backend's comparison form: invisible characters,
// spaces and . _ - dropped, lowercased, common look-alike letters folded
// (Cyrillic/Greek i, a, o, e, p, r, y, m, g; digits 1 and 0), and i folded to
// l, so "Lіgus" (Cyrillic і) or "li\u200bgus" reads as the default agent.
const LOOKALIKE: Record<string, string> = {
  "\u0456": "l", "\u0131": "l", "\u03b9": "l", "\u04cf": "l", "1": "l", "|": "l", i: "l",
  "\u0430": "a", "\u03b1": "a", "\u043e": "o", "\u03bf": "o", "0": "o", "\u0435": "e",
  "\u0440": "p", "\u03c1": "p", "\u0443": "y", "\u043c": "m", "\u0261": "g", "\u0455": "s",
};
function reservedSkeleton(name: string): string {
  return Array.from(
    name
      .replace(/[\p{Cf}\p{Cc}\s._\-\u00b7]/gu, "")
      .replace(/I/g, "l")
      .toLowerCase(),
  )
    .map((c) => LOOKALIKE[c] ?? c)
    .join("")
    .replace(/rn/g, "m");
}
