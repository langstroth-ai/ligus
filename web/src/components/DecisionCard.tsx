import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Avatar } from "@/components/Avatar";
import { Badge, Button } from "@/components/ui";
import { answerAsk, type AskChoice, type AskDto } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";

/// A decision card (docs/design/agent-questions.md 7.1): one per open
/// question. The header and the effect line are Ligus's words; the quote is
/// the agent's, shown as plain text (never Markdown, never a link). A
/// grant's buttons are Ligus's labels, and a grant needs the button itself:
/// a 2-second hold, or the passkey when its effect needs one.

/// How long a grant's Allow is held before it counts (D2).
export const HOLD_MS = 2000;

const ROLE_LABEL: Record<string, string> = { contributor: "the room", admin: "Admins", owner: "Owners" };

/// "9:41" for a ms timestamp.
function clock(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/// Who a card asks, for its header ("asks you", "asks Admins").
export function asksWhom(a: AskDto): string {
  if (a.audience.kind === "people") {
    if (a.eligible) return a.kind === "question" ? "asks you" : "asks you to allow";
    return `asks ${(a.audience.names ?? []).join(", ") || "someone"}`;
  }
  return `asks ${ROLE_LABEL[a.audience.minRole ?? "contributor"] ?? "the room"}`;
}

/// "Waiting on Ana" / "Waiting on Admins · 1 of 2".
export function waitingOn(a: AskDto): string {
  const who =
    a.audience.kind === "people" ? (a.audience.names ?? []).join(", ") || "someone" : ROLE_LABEL[a.audience.minRole ?? "contributor"] ?? "the room";
  const best = a.tally.length ? Math.max(...a.tally) : 0;
  return a.quorum > 1 ? `Waiting on ${who} · ${best} of ${a.quorum}` : `Waiting on ${who}`;
}

/// The one line a settled card collapses to.
export function settledLine(a: AskDto): string {
  const names = a.answeredBy.map((v) => v.name);
  const who = names.length <= 1 ? names[0] ?? "Someone" : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  const at = clock(a.answeredBy[a.answeredBy.length - 1]?.atMs ?? 0);
  if (a.declined) return `${who} said Not now${at ? ` · ${at}` : ""}`;
  if (a.answer != null) return `${who} answered ${a.answer}${at ? ` · ${at}` : ""}`;
  return `${a.agentName} stopped waiting.`;
}

/// Hidden on this device: an older card's "Not now" writes nothing (its
/// first answer would settle it for everyone).
const HIDE_KEY = "ligus.asks.hidden";
function hiddenHere(id: string): boolean {
  try {
    return (JSON.parse(localStorage.getItem(HIDE_KEY) ?? "[]") as string[]).includes(id);
  } catch {
    return false;
  }
}
function hideHere(id: string) {
  try {
    const v = (JSON.parse(localStorage.getItem(HIDE_KEY) ?? "[]") as string[]).filter((x) => x !== id).slice(-200);
    localStorage.setItem(HIDE_KEY, JSON.stringify([...v, id]));
  } catch {
    // Storage may be unavailable; the card just stays.
  }
}

/// A button that fires only after being held for `ms` (pointer, Space or
/// Enter). Releasing early cancels.
export function HoldButton({ ms = HOLD_MS, onHeld, children, disabled }: { ms?: number; onHeld: () => void; children: ReactNode; disabled?: boolean }) {
  const [held, setHeld] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const start = () => {
    if (disabled || timer.current) return;
    setHeld(true);
    timer.current = setTimeout(() => {
      timer.current = null;
      setHeld(false);
      onHeld();
    }, ms);
  };
  const stop = () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    setHeld(false);
  };
  useEffect(() => stop, []);
  return (
    <Button
      variant="success"
      disabled={disabled}
      aria-label={`${typeof children === "string" ? children : "Allow once"} (hold for ${ms / 1000} seconds)`}
      title="Hold to allow"
      onPointerDown={start}
      onPointerUp={stop}
      onPointerLeave={stop}
      onKeyDown={(e) => {
        if ((e.key === " " || e.key === "Enter") && !e.repeat) {
          e.preventDefault();
          start();
        }
      }}
      onKeyUp={(e) => {
        if (e.key === " " || e.key === "Enter") stop();
      }}
      data-holding={held || undefined}
      style={held ? { boxShadow: "inset 0 0 0 2px var(--ligus-success-line)" } : undefined}
    >
      {children}
      {held && <span className="sr-only">Holding…</span>}
    </Button>
  );
}

/// The card's effect detail: mono, scrolling, cut only behind an ellipsis
/// that expands.
function Detail({ text }: { text: string }) {
  const [all, setAll] = useState(false);
  const LIMIT = 600;
  const long = text.length > LIMIT;
  return (
    <div className="mt-1">
      <pre
        data-testid="ask-detail"
        className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg border px-2 py-1.5 font-mono text-xs"
        style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
      >
        {long && !all ? `${text.slice(0, LIMIT)}…` : text}
      </pre>
      {long && (
        <button type="button" className="mt-0.5 text-xs text-ink-soft underline" onClick={() => setAll((v) => !v)}>
          {all ? "Show less" : "Show all"}
        </button>
      )}
    </div>
  );
}

export function DecisionCard({ sessionId, ask, origin }: { sessionId: string; ask: AskDto; origin?: string }) {
  const qc = useQueryClient();
  const [reply, setReply] = useState<string | null>(null);
  const [picked, setPicked] = useState<number | null>(null);
  const [hidden, setHidden] = useState(() => hiddenHere(ask.askId));
  const send = useMutation({
    mutationFn: (c: AskChoice) => answerAsk(sessionId, ask.askId, c),
    onSuccess: () => {
      setReply(null);
      void qc.invalidateQueries({ queryKey: ["asks", sessionId] });
      void qc.invalidateQueries({ queryKey: ["chatAttention"] });
    },
    onError: (e) => toast.error(`Couldn't send your answer: ${errMsg(e)}`),
  });
  const grant = ask.kind !== "question" ? ask.effect ?? null : null;
  const host = grant?.hostLabel || "its host";

  if (!ask.live || hidden) {
    if (hidden && ask.live) return null;
    const by = ask.answeredBy[ask.answeredBy.length - 1];
    return (
      <div className="px-3 py-1 text-xs text-ink-soft" data-testid="ask-settled" data-tip={by?.deviceId ? `Signed by device ${by.deviceId.slice(0, 8)}` : undefined}>
        {settledLine(ask)}
      </div>
    );
  }

  const tags: string[] = [];
  if (ask.origin?.via === "subagent") tags.push("via sub-agent");
  if (ask.origin?.via === "outside") tags.push("outside Ligus");
  if (origin) tags.push(`from ${origin}`);
  if (ask.postedBy) tags.push(`posted by ${ask.postedBy}`);
  const title = ask.origin?.via === "outside" ? `${ask.agentName} (outside Ligus)` : ask.agentName;
  const choose = (c: AskChoice) => {
    if (!send.isPending) send.mutate(c);
  };
  const notNow = () => {
    if (ask.v >= 2) choose({ decline: true });
    else {
      hideHere(ask.askId);
      setHidden(true);
    }
  };
  const textAllowed = ask.allowText || (ask.v < 2 && !grant);
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!ask.eligible || grant || reply !== null) return;
    const n = Number(e.key);
    if (Number.isInteger(n) && n >= 1 && n <= Math.min(5, ask.options.length)) {
      e.preventDefault();
      setPicked(n - 1);
    } else if (e.key === "Enter" && picked !== null && e.target === e.currentTarget) {
      e.preventDefault();
      choose({ optionIndex: picked });
    }
  };

  return (
    <div
      role="group"
      aria-label={`${title} ${asksWhom(ask)}`}
      tabIndex={0}
      onKeyDown={onKey}
      className="my-1 rounded-2xl border px-3 py-2 outline-none focus-visible:ring-2"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
      data-testid="ask-card"
    >
      <div className="flex items-center gap-2" data-part="header">
        <Avatar name={ask.agentName} kind="agent" size={20} />
        <span className="text-sm">
          <span className="font-semibold">{title}</span> {asksWhom(ask)}
          {ask.quorum > 1 && (
            <span className="text-ink-soft">
              {" "}
              · needs {ask.quorum} of you · {Math.max(0, ...ask.tally)} so far
            </span>
          )}
        </span>
        {tags.map((t) => (
          <Badge key={t}>{t}</Badge>
        ))}
        <span className="ml-auto text-xs text-ink-soft">
          {grant?.hostLabel ? `${grant.hostLabel} · ` : ""}
          {clock(ask.atMs)}
        </span>
      </div>
      {/* A v2 grant's question is the host's own words (its effect line says
          it); every other card quotes the agent. */}
      {!(grant && ask.v >= 2) && (
        <blockquote data-part="quote" className="mt-1.5 whitespace-pre-wrap break-words text-sm" style={{ overflowWrap: "anywhere" }}>
          {`“${ask.question}”`}
        </blockquote>
      )}
      {grant && (
        <div data-part="effect" className="mt-1.5 text-xs text-ink-soft">
          {grant.tooLarge ? (
            <div>Too large to review here. Answer at the terminal.</div>
          ) : (
            <div>
              {grant.standing
                ? `Allow for the rest of the session lets ${ask.agentName} run calls like this on ${host} without asking again:`
                : `Allow once lets ${ask.agentName} run this once on ${host}:`}
            </div>
          )}
          <Detail text={grant.detail || grant.tool} />
          {grant.title && <div className="mt-1">{`${ask.agentName} describes it as: ${grant.title}`}</div>}
          {grant.presenceRequired && <div className="mt-1">Allowing asks for your passkey.</div>}
        </div>
      )}
      {ask.eligible ? (
        <div data-part="options" className="mt-2 flex flex-wrap items-center gap-1.5">
          {grant ? (
            <>
              {!grant.tooLarge &&
                (grant.presenceRequired ? (
                  <Button variant="success" disabled={send.isPending} onClick={() => choose({ optionIndex: 0 })}>
                    {ask.options[0]}
                  </Button>
                ) : (
                  <HoldButton disabled={send.isPending} onHeld={() => choose({ optionIndex: 0 })}>
                    {ask.options[0]}
                  </HoldButton>
                ))}
              <Button disabled={send.isPending} onClick={() => choose({ optionIndex: 1 })}>
                {ask.options[1] ?? "Deny"}
              </Button>
            </>
          ) : (
            ask.options.map((o, i) => (
              <Button
                key={`${i}:${o}`}
                disabled={send.isPending}
                aria-pressed={picked === i}
                aria-keyshortcuts={i < 5 ? String(i + 1) : undefined}
                onClick={() => choose({ optionIndex: i })}
              >
                {o}
              </Button>
            ))
          )}
          {textAllowed &&
            (reply === null ? (
              <button type="button" className="text-sm text-ink-soft underline" onClick={() => setReply("")}>
                Reply…
              </button>
            ) : (
              <form
                className="flex min-w-0 flex-1 gap-1.5"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (reply.trim()) choose({ text: reply.trim() });
                }}
              >
                <input
                  autoFocus
                  aria-label="Your reply"
                  maxLength={1000}
                  className="min-w-0 flex-1 rounded-lg border px-2 py-1 text-sm"
                  style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
                  value={reply}
                  onChange={(e) => setReply(e.target.value)}
                />
                <Button type="submit" variant="primary" disabled={!reply.trim() || send.isPending}>
                  Send
                </Button>
              </form>
            ))}
          <button type="button" className="ml-auto text-sm text-ink-soft" onClick={notNow} disabled={send.isPending}>
            Not now
          </button>
        </div>
      ) : (
        <div data-part="options" className="mt-2 text-xs text-ink-soft">
          {waitingOn(ask)}
        </div>
      )}
      {!grant && (
        <div data-part="effect" className="mt-1.5 text-xs text-ink-soft">
          {`${ask.agentName} decides what to do next. Your answer grants no new permission.`}
        </div>
      )}
      {ask.olderAnswerUncounted && <div className="mt-1 text-xs text-ink-soft">Answered on an older version; not counted.</div>}
    </div>
  );
}
