import { useSyncExternalStore } from "react";
import type { LiveTurnSnapshot } from "@/lib/sessionState";

// Which chats have a turn running, which finished while you were elsewhere,
// and which failed: the live half of a chat's session state. Fed by every
// `chat://stream` event (one app-wide subscription, see App) so a chat that
// isn't open still shows that it is working. Nothing here is persisted.

interface Entry {
  /// Message ids streaming right now.
  streaming: Set<string>;
  /// The open chat view says a turn is in flight (sent, no delta yet).
  sending: boolean;
  /// Last stream or activity event, for the staleness check.
  touchedAt: number;
  failed: string | null;
  finishedUnseen: boolean;
}

/// A turn with no event for this long is treated as gone (a missed terminal
/// event must not leave a chat "working" forever). Long tool runs emit
/// activity events, which count as events.
export const LIVE_STALE_MS = 5 * 60_000;

const entries = new Map<string, Entry>();
let viewing: string | null = null;
let snapshot: ReadonlyMap<string, LiveTurnSnapshot> = new Map();
const listeners = new Set<() => void>();
let now: () => number = () => Date.now();

function entry(id: string): Entry {
  let e = entries.get(id);
  if (!e) {
    e = { streaming: new Set(), sending: false, touchedAt: now(), failed: null, finishedUnseen: false };
    entries.set(id, e);
  }
  return e;
}

function isLive(e: Entry, at: number): boolean {
  return (e.streaming.size > 0 || e.sending) && at - e.touchedAt < LIVE_STALE_MS;
}

function publish() {
  const at = now();
  const next = new Map<string, LiveTurnSnapshot>();
  for (const [id, e] of entries) {
    const live = isLive(e, at);
    if (live || e.failed || e.finishedUnseen) next.set(id, { live, failed: e.failed, finishedUnseen: e.finishedUnseen });
  }
  snapshot = next;
  for (const l of listeners) l();
}

/// When the last live turn of a chat ends: remember it finished, unless the
/// chat is the one on screen (you watched it finish).
function settle(id: string, e: Entry) {
  if (e.streaming.size === 0 && !e.sending && id !== viewing && !e.failed) e.finishedUnseen = true;
}

export const liveTurns = {
  /// A `chat://stream` event: "delta", "completed" or "error".
  onStream(ev: { sessionId: string; messageId: string; phase: string; text: string }) {
    const e = entry(ev.sessionId);
    e.touchedAt = now();
    if (ev.phase === "delta") {
      e.streaming.add(ev.messageId);
      e.failed = null;
      e.finishedUnseen = false;
    } else {
      const wasLive = e.streaming.delete(ev.messageId) || e.sending;
      if (ev.phase === "error") e.failed = ev.text?.trim() || "Generation failed or was interrupted.";
      if (wasLive || ev.phase === "completed") settle(ev.sessionId, e);
    }
    publish();
  },
  /// A `chat://activity` event (tool call, result, thinking): the turn is alive.
  onActivity(sessionId: string) {
    const e = entries.get(sessionId);
    if (!e) return;
    e.touchedAt = now();
  },
  /// The open chat view's own in-flight flag (covers the gap between send and
  /// the first delta).
  setSending(sessionId: string, sending: boolean) {
    const e = entry(sessionId);
    if (e.sending === sending) return;
    e.sending = sending;
    if (sending) {
      e.touchedAt = now();
      e.failed = null;
      e.finishedUnseen = false;
    }
    publish();
  },
  /// The person stopped the turn: nothing is running any more, and a stop is
  /// neither a failure nor news.
  stopped(sessionId: string) {
    const e = entry(sessionId);
    e.streaming.clear();
    e.sending = false;
    e.finishedUnseen = false;
    publish();
  },
  /// The chat now on screen (null: none). Opening a chat clears its "done".
  setViewing(sessionId: string | null) {
    viewing = sessionId;
    const e = sessionId ? entries.get(sessionId) : undefined;
    if (e?.finishedUnseen) {
      e.finishedUnseen = false;
      publish();
    }
  },
  failureOf(sessionId: string): string | null {
    return entries.get(sessionId)?.failed ?? null;
  },
  /// The person dismissed the failure (or sent again).
  clearFailure(sessionId: string) {
    const e = entries.get(sessionId);
    if (!e?.failed) return;
    e.failed = null;
    publish();
  },
  /// Re-evaluate staleness (called on a slow timer by the subscriber).
  refresh() {
    publish();
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
  snapshot(): ReadonlyMap<string, LiveTurnSnapshot> {
    return snapshot;
  },
  /// Tests only.
  _reset(clock?: () => number) {
    entries.clear();
    viewing = null;
    now = clock ?? (() => Date.now());
    snapshot = new Map();
  },
};

/// The live turn state of every chat, re-rendering on change.
export function useLiveTurns(): ReadonlyMap<string, LiveTurnSnapshot> {
  return useSyncExternalStore(liveTurns.subscribe, liveTurns.snapshot, liveTurns.snapshot);
}
