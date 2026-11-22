// Time left on something that expires (a pairing code, a vote's deadline):
// a small ring plus the time as text, so it reads without the ring. It ticks
// once a second; screen readers hear it only at the minute marks and the last
// ten seconds, not every tick.

import { useEffect, useRef, useState } from "react";

/// "4:05", "0:09", "1:02:00".
export function formatRemaining(ms: number): string {
  const s = Math.max(0, Math.ceil(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

export function Countdown({
  expiresAt,
  totalMs,
  onExpire,
  label = "Expires in",
  expiredText = "Expired",
  ring = true,
  now: nowFn = Date.now,
}: {
  /// When it expires, in ms since the epoch.
  expiresAt: number;
  /// The full duration, for the ring; without it the ring is hidden.
  totalMs?: number;
  onExpire?: () => void;
  label?: string;
  expiredText?: string;
  ring?: boolean;
  /// The clock (tests pass a fake).
  now?: () => number;
}) {
  const [now, setNow] = useState(nowFn);
  const fired = useRef(false);
  const left = expiresAt - now;

  useEffect(() => {
    const t = setInterval(() => setNow(nowFn()), 1000);
    return () => clearInterval(t);
  }, [nowFn]);

  useEffect(() => {
    if (left <= 0 && !fired.current) {
      fired.current = true;
      onExpire?.();
    }
    if (left > 0) fired.current = false;
  }, [left, onExpire]);

  const secs = Math.max(0, Math.ceil(left / 1000));
  const text = left <= 0 ? expiredText : formatRemaining(left);
  const announce = left <= 0 || secs <= 10 || secs % 60 === 0;
  const frac = totalMs ? Math.max(0, Math.min(1, left / totalMs)) : null;
  const R = 6.5;
  const C = 2 * Math.PI * R;
  const low = left > 0 && totalMs ? left / totalMs < 0.2 : false;

  return (
    <span className="inline-flex items-center gap-1.5 text-[12px] tabular-nums" style={{ color: low ? "var(--ligus-warn-ink)" : "var(--ligus-ink-soft)" }}>
      {ring && frac !== null && (
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden>
          <circle cx="8" cy="8" r={R} fill="none" stroke="var(--ligus-line)" strokeWidth="2" />
          <circle
            cx="8"
            cy="8"
            r={R}
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeDasharray={C}
            strokeDashoffset={C * (1 - frac)}
            transform="rotate(-90 8 8)"
            strokeLinecap="round"
          />
        </svg>
      )}
      <span role="timer" aria-live={announce ? "polite" : "off"} aria-atomic="true">
        {left > 0 && <span className="sr-only">{label} </span>}
        {text}
      </span>
    </span>
  );
}
