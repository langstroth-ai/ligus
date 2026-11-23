// One state per chat, shared by every surface that shows where a chat stands
// (the sidebar's chat rows today; the palette, badges and notifications can
// read the same answer). It is derived only from data the app already has:
// live turns (chat://stream), agents' open questions, proposals waiting on a
// decision, queued work for a host, and a lockdown.
//
// The glyph carries the meaning, so a state never depends on colour alone.

import type { QueuedWorkDto } from "@/bindings/QueuedWorkDto";

export type SessionState = "idle" | "working" | "needsYou" | "queued" | "done" | "failed" | "paused";

/// Everything known about one chat that bears on its state.
export interface SessionSignals {
  /// A turn is streaming (or was just sent) in this chat.
  live?: boolean;
  /// The last turn died mid-generation and nobody has dismissed it.
  failed?: boolean;
  /// A turn finished while the chat wasn't open.
  finishedUnseen?: boolean;
  /// Agent questions still waiting for an answer.
  openAsks?: number;
  /// Proposals waiting on this person's vote, or approved and ready to apply.
  proposalsNeedingMe?: number;
  /// Unanswered agent mentions waiting on a host.
  queued?: number;
  /// A lockdown holds the chat's agents until a person clears it.
  paused?: boolean;
}

/// Precedence, highest first. Something waiting on you outranks everything:
/// an agent that asked a question mid-turn is stopped until you answer. A
/// failure is next (it won't fix itself), then a lockdown, then live work.
/// Queued work only shows while nothing is running, and "done" is the
/// quietest: it clears as soon as you open the chat.
export function sessionState(s: SessionSignals): SessionState {
  if ((s.openAsks ?? 0) > 0 || (s.proposalsNeedingMe ?? 0) > 0) return "needsYou";
  if (s.failed) return "failed";
  if (s.paused) return "paused";
  if (s.live) return "working";
  if ((s.queued ?? 0) > 0) return "queued";
  if (s.finishedUnseen) return "done";
  return "idle";
}

export interface SessionStateMeta {
  /// The word shown next to the glyph.
  word: string;
  /// A longer description for the tooltip and screen readers.
  describe: string;
  /// Ink colour for the glyph and word (a CSS value). A dark surface (the
  /// sidebar) re-points the --ligus-state-* variables to light inks.
  ink: string;
}

export const SESSION_STATE_META: Record<Exclude<SessionState, "idle">, SessionStateMeta> = {
  working: { word: "Working", describe: "An agent is working in this chat", ink: "var(--ligus-state-working, var(--ligus-info-ink))" },
  needsYou: { word: "Needs you", describe: "An agent asked a question or a change waits on your decision", ink: "var(--ligus-state-needs-you, var(--ligus-warm-ink))" },
  queued: { word: "Queued", describe: "Waiting for the agent's host to pick it up", ink: "var(--ligus-state-queued, var(--ligus-warn-ink))" },
  done: { word: "Done", describe: "Finished since you last looked", ink: "var(--ligus-state-done, var(--ligus-success-ink))" },
  failed: { word: "Failed", describe: "The last turn failed", ink: "var(--ligus-state-failed, var(--ligus-danger-ink))" },
  paused: { word: "Paused", describe: "Agents are paused here until a person clears the lockdown", ink: "var(--ligus-state-paused, var(--ligus-ink-soft))" },
};

/// What one chat is waiting on, as `list_chat_attention` returns it.
export interface ChatAttention {
  sessionId: string;
  openAsks: number;
  proposals: { id: string; title: string; status: string; needsMe: boolean }[];
  paused: boolean;
}

/// A live turn snapshot for one chat (see state/liveTurns).
export interface LiveTurnSnapshot {
  live: boolean;
  failed: string | null;
  finishedUnseen: boolean;
}

/// Combine the sources into a state per chat id. Chats with nothing going on
/// are absent from the map (they are idle).
export function sessionStates(
  chatIds: readonly string[],
  sources: {
    attention?: readonly ChatAttention[];
    queued?: readonly QueuedWorkDto[];
    live?: ReadonlyMap<string, LiveTurnSnapshot>;
  },
): Map<string, SessionState> {
  const attention = new Map((sources.attention ?? []).map((a) => [a.sessionId, a]));
  const queued = new Map<string, number>();
  for (const q of sources.queued ?? []) queued.set(q.sessionId, (queued.get(q.sessionId) ?? 0) + 1);
  const out = new Map<string, SessionState>();
  for (const id of chatIds) {
    const a = attention.get(id);
    const l = sources.live?.get(id);
    const state = sessionState({
      live: l?.live,
      failed: Boolean(l?.failed),
      finishedUnseen: l?.finishedUnseen,
      openAsks: a?.openAsks,
      proposalsNeedingMe: a?.proposals.filter((p) => p.needsMe).length,
      queued: queued.get(id),
      paused: a?.paused,
    });
    if (state !== "idle") out.set(id, state);
  }
  return out;
}
