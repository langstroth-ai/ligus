// The status bar shell: one quiet 24px strip along the bottom of the window
// for state (sync, relay, updates, agents running). Presentational only:
// whoever renders it decides which items it shows. Items are words with an
// optional glyph, never colour alone; a tone only tints the glyph.

import type { ReactNode } from "react";
import type { Tone } from "@/components/ui";

const TONE_INK: Record<Tone, string> = {
  neutral: "var(--ligus-ink-soft)",
  info: "var(--ligus-info-ink)",
  success: "var(--ligus-success-ink)",
  warn: "var(--ligus-warn-ink)",
  danger: "var(--ligus-danger-ink)",
};

export function StatusBar({
  start,
  end,
  hidden = false,
  label = "Status",
}: {
  /// Items on the left (sync, relay).
  start?: ReactNode;
  /// Items on the right (updates, agents running).
  end?: ReactNode;
  /// Focus mode hides it; it stays mounted so live regions keep working.
  hidden?: boolean;
  label?: string;
}) {
  return (
    <footer
      aria-label={label}
      hidden={hidden}
      className="flex h-6 shrink-0 items-center gap-3 border-t px-3 text-[11.5px]"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)", color: "var(--ligus-ink-soft)" }}
    >
      <div className="flex min-w-0 flex-1 items-center gap-3">{start}</div>
      <div className="flex shrink-0 items-center gap-3">{end}</div>
    </footer>
  );
}

export function StatusBarItem({
  icon,
  children,
  tone = "neutral",
  onClick,
  describe,
  live = false,
}: {
  icon?: ReactNode;
  children: ReactNode;
  tone?: Tone;
  /// Makes the item a button (open the sync panel, the update notes).
  onClick?: () => void;
  /// A longer accessible description ("Connected to relay.example.com").
  describe?: string;
  /// Announce changes politely (sync connect and disconnect).
  live?: boolean;
}) {
  const body = (
    <>
      {icon && (
        <span aria-hidden className="inline-flex" style={{ color: TONE_INK[tone] }}>
          {icon}
        </span>
      )}
      <span className="truncate">{children}</span>
    </>
  );
  const cls = "inline-flex h-6 min-w-0 items-center gap-1 rounded px-1";
  if (onClick) {
    return (
      <button type="button" onClick={onClick} aria-description={describe} className={`ligus-press ${cls}`} style={{ color: "inherit" }}>
        {body}
      </button>
    );
  }
  return (
    <span className={cls} aria-description={describe} role={live ? "status" : undefined} aria-live={live ? "polite" : undefined}>
      {body}
    </span>
  );
}
