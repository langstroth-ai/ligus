// A name or title someone else chose, as the UI shows it. Mirrors
// `ligus_proto::names::clean_display_name`: bidi embeddings, overrides,
// isolates and marks, zero-width and other invisible format characters,
// blank fillers and control characters are removed, whitespace (line and
// paragraph separators included) collapses to one space, and the ends are
// trimmed. The desktop already cleans what it sends; this guards the text
// the UI builds around a name (a confirm dialog) as well.
const HIDDEN =
  /[\u00AD\u034F\u061C\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B-\u200F\u202A-\u202E\u2060-\u206F\u3164\uFE00-\uFE0F\uFEFF\uFFA0\uFFF9-\uFFFB\u{1D173}-\u{1D17A}\u{E0000}-\u{E0FFF}\p{Cc}]/gu;

export function cleanDisplayName(name: string): string {
  return name.replace(HIDDEN, "").replace(/\s+/gu, " ").trim();
}
