// A small subsequence scorer for the command palette: every query character
// must appear in order in the text. Higher is better; null means no match.
//
// Scoring favours what people type when they know what they want: a match at
// the start of the text or of a word ("os" → "Open settings"), runs of
// consecutive characters, and short texts. Gaps cost a little.

const WORD_BREAK = /[\s\-_/:.·@#()"“”]/;

export function fuzzyScore(query: string, text: string): number | null {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const t = text.toLowerCase();
  // A plain substring is the strongest signal; score it on its own scale so
  // it always beats a scattered subsequence.
  const at = t.indexOf(q);
  if (at >= 0) {
    const wordStart = at === 0 || WORD_BREAK.test(t[at - 1]);
    return 1000 + (at === 0 ? 200 : 0) + (wordStart ? 100 : 0) - at - t.length / 10;
  }
  let score = 0;
  let ti = 0;
  let prev = -2;
  for (const ch of q) {
    if (ch === " ") continue;
    const found = t.indexOf(ch, ti);
    if (found < 0) return null;
    const wordStart = found === 0 || WORD_BREAK.test(t[found - 1]);
    if (found === prev + 1) score += 8;
    else if (wordStart) score += 6;
    else score += 1;
    if (prev >= 0 && found > prev + 1) score -= Math.min(4, (found - prev - 1) / 3);
    prev = found;
    ti = found + 1;
  }
  return score - t.length / 20;
}

/// Rank items by score against `query`, dropping non-matches. Ties keep
/// their input order (so callers can pre-sort by recency or context).
export function fuzzyRank<T>(items: readonly T[], query: string, text: (item: T) => string, bonus?: (item: T) => number): T[] {
  const scored: { item: T; score: number; i: number }[] = [];
  items.forEach((item, i) => {
    const s = fuzzyScore(query, text(item));
    if (s !== null) scored.push({ item, score: s + (bonus?.(item) ?? 0), i });
  });
  scored.sort((a, b) => b.score - a.score || a.i - b.i);
  return scored.map((s) => s.item);
}
