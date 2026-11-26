// Where a turn's decision cards sit around its reply bubble.
//
// A card belongs to the turn that asked (its `turnId` is the reply message).
// The reply is one bubble posted when the turn finishes, so a card the person
// already settled reads before the reply that reports the answer; a card
// still waiting (or a reply still streaming, or one posted before the answer,
// as an older build's replies were stamped) stays after it, next to the
// composer.

import type { AskDto } from "@/lib/ipc";

/// When a card was settled: its latest counted answer, else when it was asked
/// (a card closed or expired without an answer settled no later than the turn
/// that asked it moved on).
export function settledAtMs(a: AskDto): number {
  return a.answeredBy.reduce((t, v) => Math.max(t, v.atMs || 0), 0) || a.atMs;
}

/// Split a turn's cards into those drawn before its reply and those after.
/// `reply.createdAt` is when the reply was posted (its completion).
export function placeTurnAsks(
  asks: AskDto[] | undefined,
  reply: { createdAt?: string | null; isStreaming?: boolean },
): { before: AskDto[]; after: AskDto[] } {
  const list = asks ?? [];
  const posted = reply.createdAt ? Date.parse(reply.createdAt) : NaN;
  if (reply.isStreaming || Number.isNaN(posted)) return { before: [], after: list };
  const before: AskDto[] = [];
  const after: AskDto[] = [];
  for (const a of list) (!a.live && settledAtMs(a) <= posted ? before : after).push(a);
  return { before, after };
}
