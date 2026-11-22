// A session's state and its card. Presentational: callers pass the state
// (lib/sessionState) and whatever they know; nothing here fetches.

import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { SessionStateChip } from "@/components/ui";
import { SESSION_STATE_META, type SessionState } from "@/lib/sessionState";
import "./kit.css";

/// The state chip plus an optional elapsed time ("Working · 0:42"), and a
/// polite announcement for screen readers when the state changes (not on
/// first render, and never per token).
export function SessionStatus({
  state,
  elapsed,
  announce = false,
  label,
}: {
  state: SessionState | undefined;
  /// Shown after the word, e.g. "0:42" for a running turn.
  elapsed?: string;
  /// Announce state changes in a polite live region.
  announce?: boolean;
  /// What the announcement names, e.g. the chat title: "Fix flaky test: Done".
  label?: string;
}) {
  const [message, setMessage] = useState("");
  const prev = useRef(state);
  useEffect(() => {
    if (!announce || prev.current === state) return;
    prev.current = state;
    const word = state && state !== "idle" ? SESSION_STATE_META[state].word : "Idle";
    setMessage(label ? `${label}: ${word}` : word);
  }, [announce, state, label]);

  if (!state || state === "idle") {
    return announce ? <span role="status" aria-live="polite" className="sr-only">{message}</span> : null;
  }
  return (
    <span className="inline-flex shrink-0 items-center gap-1">
      <SessionStateChip state={state} />
      {elapsed && (
        <span className="text-[11px] tabular-nums" style={{ color: "var(--ligus-ink-soft)" }}>
          · {elapsed}
        </span>
      )}
      {announce && (
        <span role="status" aria-live="polite" className="sr-only">
          {message}
        </span>
      )}
    </span>
  );
}

/// One session (a chat and where it stands) as a card: title, state, one
/// meta line ("Runs on MacBook Pro · claude-code / opus") and an optional
/// detail line and people. A state change gets one short pulse (none with
/// reduced motion).
export function SessionCard({
  title,
  state,
  meta,
  detail,
  people,
  when,
  active = false,
  onOpen,
  actions,
}: {
  title: string;
  state?: SessionState;
  meta?: ReactNode;
  detail?: ReactNode;
  people?: ReactNode;
  when?: string;
  active?: boolean;
  onOpen?: () => void;
  /// Trailing controls; clicks on them don't open the card.
  actions?: ReactNode;
}) {
  const [pulse, setPulse] = useState(false);
  const prev = useRef(state);
  useEffect(() => {
    if (prev.current === state) return;
    prev.current = state;
    setPulse(true);
    // Drop the class after the pulse so the next change can replay it (a
    // timer, not animationend: with reduced motion no animation runs).
    const t = setTimeout(() => setPulse(false), 400);
    return () => clearTimeout(t);
  }, [state]);

  return (
    <div
      data-session-card
      data-active={active || undefined}
      className={`ligus-row group/scard relative rounded-xl border ${pulse ? "ligus-state-pulse" : ""}`}
      style={{ borderColor: "var(--ligus-line)", background: active ? undefined : "var(--ligus-panel)" }}
    >
      <button
        type="button"
        onClick={onOpen}
        aria-current={active ? "true" : undefined}
        className="flex w-full flex-col gap-1 px-3.5 py-3 text-left"
      >
        <span className="flex items-center gap-2">
          <span className="min-w-0 flex-1 truncate text-[13px] font-semibold" style={{ color: "var(--ligus-ink)" }}>
            {title}
          </span>
          {state && state !== "idle" ? (
            <SessionStateChip state={state} />
          ) : (
            when && (
              <span className="shrink-0 text-[11px] tabular-nums" style={{ color: "var(--ligus-ink-soft)" }}>
                {when}
              </span>
            )
          )}
        </span>
        {meta && (
          <span className="min-w-0 truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {meta}
          </span>
        )}
        {(detail || people) && (
          <span className="flex items-center gap-2 text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {people}
            {detail && <span className="min-w-0 flex-1 truncate">{detail}</span>}
          </span>
        )}
      </button>
      {actions && <div className="flex items-center justify-end gap-2 px-3.5 pb-3">{actions}</div>}
    </div>
  );
}
