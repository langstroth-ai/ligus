/// Composer @-mention helpers, factored out of ChatView so they're unit-testable.

export interface MentionCandidate {
  handle: string;
  kind: string;
  /// Set when the viewer can't use this agent (agent access controls): the
  /// row stays listed, greyed out with this reason, and can't be inserted.
  disabledReason?: string;
}

/// The @mention candidates for roster agents. An agent this person can't
/// use (agent access controls: `available === false`) stays listed, greyed
/// out with the reason, and can't be inserted.
export function agentMentionCandidates(
  agents: { name: string; available?: boolean; unavailableReason?: string | null }[],
): MentionCandidate[] {
  return agents.map((a) =>
    a.available === false
      ? { handle: a.name, kind: "Not available", disabledReason: a.unavailableReason || "You can't use this agent" }
      : { handle: a.name, kind: "Agent" },
  );
}

/// Whether a candidate can be inserted into the draft.
export function mentionSelectable(c: MentionCandidate | undefined): boolean {
  return Boolean(c) && !c!.disabledReason;
}

/// Detect an `@token` immediately before the caret. The token is a single word
/// (`[\w-]*`) that must start at the beginning of input or after whitespace, so
/// `email@x` doesn't trigger it. Returns the `@`'s index and the partial query.
export function detectMention(value: string, caret: number): { start: number; query: string } | null {
  const before = value.slice(0, caret);
  const m = /(^|\s)@([\w-]*)$/.exec(before);
  if (!m) return null;
  return { start: caret - m[2].length - 1, query: m[2] };
}

/// Candidates whose handle starts with the (case-insensitive) query.
export function filterMentions(
  cands: MentionCandidate[],
  query: string,
  limit = 8,
): MentionCandidate[] {
  const q = query.toLowerCase();
  return cands.filter((c) => c.handle.toLowerCase().startsWith(q)).slice(0, limit);
}

/// Split `text` around each `@you` mention (case-insensitive), with the same
/// boundaries the runtime's mention parser uses: the `@` must not follow a
/// word character (so `me@you.io` is an address) and `you` must not run on
/// into a longer word (`@young`). Returns the text pieces with `null` where a
/// mention stood.
export function splitYouMentions(text: string): (string | null)[] {
  const out: (string | null)[] = [];
  const re = /@you(?![\p{L}\p{N}_-])/giu;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const before = m.index > 0 ? text[m.index - 1] : "";
    if (before && /[\p{L}\p{N}_]/u.test(before)) continue;
    out.push(text.slice(last, m.index), null);
    last = m.index + m[0].length;
  }
  out.push(text.slice(last));
  return out.filter((p) => p !== "");
}

/// Hidden and control characters removed, whitespace runs collapsed, trimmed
/// — the display-name rule used for every name the UI shows.
export function cleanName(name: string): string {
  return name
    .replace(/[\p{Cc}\p{Cf}͏ᅟᅠ឴឵᠋-᠏ㅤ︀-️ﾠ]/gu, "")
    .replace(/\s+/g, " ")
    .trim();
}

interface MdNode {
  type: string;
  value?: string;
  children?: MdNode[];
  data?: Record<string, unknown>;
}

/// A remark plugin that renders an agent reply's `@you` as `@<requester>`
/// with mention styling (`ligus-mention`, plus `ligus-mention-me` when the
/// viewer is the requester). Display only: the message body is not changed.
/// Code spans, code blocks and links are left alone.
export function remarkYouMention(opts: { name: string; me: boolean }) {
  const name = cleanName(opts.name);
  return () => (tree: MdNode) => {
    if (!name) return;
    const walk = (node: MdNode) => {
      if (!node.children) return;
      const next: MdNode[] = [];
      for (const child of node.children) {
        if (child.type === "text" && child.value && /@you/i.test(child.value)) {
          for (const part of splitYouMentions(child.value)) {
            if (part === null) {
              next.push({
                type: "youMention",
                data: {
                  hName: "span",
                  hProperties: {
                    className: opts.me ? ["ligus-mention", "ligus-mention-me"] : ["ligus-mention"],
                    title: opts.me ? "Mentions you" : `Mentions ${name}`,
                  },
                  hChildren: [{ type: "text", value: `@${name}` }],
                },
              });
            } else {
              next.push({ type: "text", value: part });
            }
          }
        } else {
          if (child.type !== "link" && child.type !== "linkReference") walk(child);
          next.push(child);
        }
      }
      node.children = next;
    };
    walk(tree);
  };
}
