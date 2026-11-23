// Who is here. Presence is carried by shape as well as colour: a filled dot
// is active now, a ring is away, a dashed ring is offline. The words are in
// the accessible name and the hover text.

import type { ReactNode } from "react";
import { Avatar } from "@/components/Avatar";

export type Presence = "active" | "away" | "offline";

const PRESENCE_WORD: Record<Presence, string> = { active: "Active now", away: "Away", offline: "Offline" };

/// The categorical colour for the nth person (1..8, wraps), 3:1 on panel.
export function presenceColor(index: number): string {
  const n = (((index - 1) % 8) + 8) % 8 + 1;
  return `var(--ligus-presence-${n}, var(--ligus-accent-cool))`;
}

export function PresenceDot({
  presence,
  name,
  color = "var(--ligus-success)",
  size = 8,
}: {
  presence: Presence;
  /// Who it is for, so the label reads "Maya: Away".
  name?: string;
  color?: string;
  size?: number;
}) {
  const word = PRESENCE_WORD[presence];
  const label = name ? `${name}: ${word}` : word;
  const ring = presence === "active" ? undefined : `1.5px ${presence === "offline" ? "dashed" : "solid"} ${presence === "offline" ? "var(--ligus-ink-faint)" : color}`;
  return (
    <span
      role="img"
      aria-label={label}
      data-tip={label}
      data-presence={presence}
      className="inline-block shrink-0 rounded-full"
      style={{
        width: size,
        height: size,
        background: presence === "active" ? color : "transparent",
        border: ring,
        boxSizing: "border-box",
        boxShadow: presence === "active" ? "0 0 0 2px var(--ligus-panel)" : undefined,
      }}
    />
  );
}

export interface StackPerson {
  name: string;
  avatarUrl?: string | null;
  colorHex?: string | null;
  kind?: "human" | "agent";
  presence?: Presence;
}

/// Overlapping avatars for the people in a session, most relevant first,
/// with "+N" for the rest. The list of names is the accessible name.
export function AvatarStack({
  people,
  max = 4,
  size = 22,
  trailing,
}: {
  people: StackPerson[];
  max?: number;
  size?: number;
  trailing?: ReactNode;
}) {
  const shown = people.slice(0, max);
  const rest = people.length - shown.length;
  const names = people.map((p) => (p.presence ? `${p.name} (${PRESENCE_WORD[p.presence].toLowerCase()})` : p.name)).join(", ");
  return (
    <span role="img" aria-label={names} data-tip={people.map((p) => p.name).join(", ")} className="inline-flex items-center">
      {shown.map((p, i) => (
        <span
          key={`${p.name}-${i}`}
          aria-hidden
          className="relative inline-flex rounded-full"
          style={{ marginLeft: i === 0 ? 0 : -Math.round(size * 0.3), boxShadow: "0 0 0 2px var(--ligus-panel)", borderRadius: p.kind === "agent" ? "30%" : "50%" }}
        >
          <Avatar name={p.name} url={p.avatarUrl} colorHex={p.colorHex} kind={p.kind} size={size} />
          {p.presence && (
            <span className="absolute -bottom-0.5 -right-0.5 inline-flex">
              <PresenceDot presence={p.presence} size={Math.max(6, Math.round(size * 0.34))} />
            </span>
          )}
        </span>
      ))}
      {rest > 0 && (
        <span
          aria-hidden
          className="grid place-items-center rounded-full text-[11px] font-semibold tabular-nums"
          style={{
            marginLeft: -Math.round(size * 0.3),
            minWidth: size,
            height: size,
            padding: "0 4px",
            background: "var(--ligus-mist)",
            color: "var(--ligus-ink-soft)",
            boxShadow: "0 0 0 2px var(--ligus-panel)",
          }}
        >
          +{rest}
        </span>
      )}
      {trailing}
    </span>
  );
}
