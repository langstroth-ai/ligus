import type { PresenceDto } from "@/lib/ipc";

/// Names of the people typing in `sessionId`, from a presence listing (self and
/// stale entries are already excluded by the backend). One name per person;
/// a nameless entry reads "Someone".
export function typingNamesFor(presence: PresenceDto[] | undefined, sessionId: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const p of presence ?? []) {
    if (!p.typing || p.sessionId !== sessionId || seen.has(p.actorId)) continue;
    seen.add(p.actorId);
    out.push(p.name.trim() || "Someone");
  }
  return out;
}

export function typingLabel(names: string[]): string {
  if (names.length === 1) return `${names[0]} is typing`;
  if (names.length === 2) return `${names[0]} and ${names[1]} are typing`;
  return "Several people are typing";
}

/// Animated "… is typing"/"is thinking" row, shown as a lightweight pending
/// bubble so it reads as an in-flight turn rather than floating text.
export function TypingDots({ label }: { label: string }) {
  return (
    <div
      className="inline-flex items-center gap-2 rounded-2xl border px-3.5 py-2.5 text-sm"
      style={{ borderColor: "color-mix(in srgb, var(--ligus-accent-cool) 26%, transparent)", background: "color-mix(in srgb, var(--ligus-accent-cool) 10%, transparent)" }}
    >
      <span className="text-ink-soft">{label}</span>
      <span className="inline-flex gap-0.5">
        <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-60 [animation-delay:-0.2s]" />
        <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-60 [animation-delay:-0.1s]" />
        <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-60" />
      </span>
    </div>
  );
}

/// Other people typing in this chat (from the desktop, a phone through its
/// box, …). Distinct from the agent's "is thinking" row. Announced politely
/// to screen readers.
export function PeopleTyping({ presence, sessionId }: { presence: PresenceDto[] | undefined; sessionId: string }) {
  const names = typingNamesFor(presence, sessionId);
  if (names.length === 0) return null;
  const label = typingLabel(names);
  return (
    <div role="status" aria-live="polite" aria-label={label} data-testid="people-typing">
      <TypingDots label={label} />
    </div>
  );
}
