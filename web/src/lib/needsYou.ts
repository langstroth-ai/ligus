// What needs *you*, as one list: the Inbox shows it, and the dock (and tray)
// badge counts it. Built only from data the app already has (the session
// state sources plus invites and device requests), so every surface agrees.
//
// Only things waiting on this person count. News, activity, suggestions and
// totals never do.

import type { ChatAttention, LiveTurnSnapshot } from "@/lib/sessionState";

export type NeedsYouKind =
  /// An agent stopped mid-turn to ask you something.
  | "ask"
  /// A proposal waits on your vote.
  | "vote"
  /// A proposal reached its approvals and waits to be applied.
  | "apply"
  /// A run in this chat failed on this device.
  | "failed"
  /// A workspace or direct-message invite delivered to this device.
  | "invite"
  /// A box says it is your device and waits for you to approve it.
  | "device"
  /// Another phone answered the add-a-phone code you showed.
  | "phone"
  /// A box asks to join, or a device asks to be yours: compare a code.
  | "approval"
  /// A request that didn't match, or a computer link that didn't finish
  /// (a day; informational, not counted).
  | "approvalFact"
  /// The open repository ships commands that stay inert until you enable them.
  | "trust"
  /// The workspace's sandbox grants changed since you last looked.
  | "sandbox"
  /// An agent's question to someone else (shown only when nothing else
  /// waits; never counted).
  | "waiting";

export interface NeedsYouItem {
  /// Stable across refetches (for React keys and "seen" bookkeeping).
  key: string;
  kind: NeedsYouKind;
  /// One line, plain words.
  title: string;
  /// The chat it belongs to, if any (opens it).
  chatId?: string;
  /// The chat's title, shown as where it is.
  chatTitle?: string;
  /// The proposal, for votes and applies.
  proposalId?: string;
  /// How many questions an "ask" row stands for.
  count?: number;
  /// The question, for an "ask" or "waiting" row.
  askId?: string;
  /// An approval request (or, collapsed, several).
  approvals?: readonly ApprovalRef[];
  /// When it was asked or happened (ms), shown muted on the row.
  at?: number;
  /// An approval fact's kind and its key (to dismiss it).
  fact?: { kind: "mismatch" | "didntFinish"; key: string; help?: string };
}

export interface ApprovalRef {
  workspaceId: string;
  rid: string;
  kind: "box" | "computer" | "phone";
  label: string;
  createdAt: number;
}

/// More approval requests than this collapse into one row.
export const APPROVAL_COLLAPSE = 3;

/// The longest label shown (the runtime's hint() bound).
export const MAX_LABEL_CHARS = 64;

/// A requester's label as every approval surface shows it: invisible and
/// direction-changing characters removed, spaces collapsed, at most 64
/// characters, in quotes, so it can't pass for the app's own words.
export function quoteLabel(raw: string): string {
  const clean = raw
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u202a-\u202e\u2060-\u2069\ufeff]/g, "")
    .replace(/\s+/g, " ")
    .trim();
  const chars = [...clean];
  const shown = chars.length > MAX_LABEL_CHARS ? chars.slice(0, MAX_LABEL_CHARS - 1).join("") + "…" : clean;
  return `\u201C${shown || "unnamed"}\u201D`;
}

/// Verb first, from the request's kind; the label last and quoted:
/// "A box asks to join: “build-1”".
export function approvalTitle(a: { kind: string; label: string }): string {
  const what = a.kind === "box" ? "A box asks to join" : a.kind === "phone" ? "A phone asks to be your device" : "A computer asks to be your device";
  return `${what}: ${quoteLabel(a.label)}`;
}

/// Most urgent first: security checks (a phone alert, then changed sandbox
/// grants and repository commands waiting for trust), then what blocks an
/// agent and decisions, then failures, then requests from outside. The
/// security rows sit above asks and votes so another member can't bury them
/// under a pile of those.
const ORDER: Record<NeedsYouKind, number> = {
  phone: 0,
  approval: 1,
  approvalFact: 2,
  sandbox: 3,
  trust: 4,
  ask: 5,
  vote: 6,
  apply: 7,
  failed: 8,
  device: 9,
  invite: 10,
  waiting: 11,
};

export interface NeedsYouSources {
  attention?: readonly ChatAttention[];
  live?: ReadonlyMap<string, LiveTurnSnapshot>;
  /// Chat id → title, for naming where an item is.
  chatTitles?: ReadonlyMap<string, string>;
  invites?: readonly { relay: string; seq: number; isDm: boolean; workspaceName: string; founderName: string | null }[];
  deviceRequests?: readonly { hostId: string; label: string }[];
  phoneAlerts?: readonly { rid: string; otherDeviceId: string; addedLabel: string }[];
  /// Repository commands waiting to be enabled on this device.
  trustWaiting?: number;
  /// The sandbox grants changed and nobody reviewed it here yet.
  sandboxChanged?: boolean;
  /// Code-match requests this device can approve.
  approvals?: readonly ApprovalRef[];
  /// Approval facts of the last day.
  approvalFacts?: readonly { key: string; kind: "mismatch" | "didntFinish"; label: string; at: number; text?: string; help?: string }[];
}

function plural(n: number, one: string, many: string) {
  return n === 1 ? one : many;
}

/// Everything waiting on this person, most urgent first.
export function needsYouItems(s: NeedsYouSources): NeedsYouItem[] {
  const out: NeedsYouItem[] = [];
  const waitingOnOthers: NeedsYouItem[] = [];
  const titleOf = (id: string) => {
    const t = s.chatTitles?.get(id);
    return t && t.trim() ? t : "Untitled chat";
  };
  for (const a of s.phoneAlerts ?? []) {
    out.push({
      key: `phone:${a.rid}:${a.otherDeviceId}`,
      kind: "phone",
      title: `Another phone answered the code you showed for ${a.addedLabel || "your phone"}`,
    });
  }
  const approvals = [...(s.approvals ?? [])].sort((a, b) => a.createdAt - b.createdAt);
  if (approvals.length > APPROVAL_COLLAPSE) {
    out.push({ key: "approval:several", kind: "approval", title: `${approvals.length} requests to approve`, approvals, at: approvals[approvals.length - 1].createdAt });
  } else {
    for (const a of approvals) {
      out.push({ key: `approval:${a.rid}`, kind: "approval", title: approvalTitle(a), approvals: [a], at: a.createdAt });
    }
  }
  for (const f of s.approvalFacts ?? []) {
    out.push({
      key: `approvalFact:${f.key}`,
      kind: "approvalFact",
      title:
        f.kind === "mismatch"
          ? `Request from ${quoteLabel(f.label)} didn't match. Nothing was granted.`
          : `Linking ${quoteLabel(f.label)} didn't finish. Nothing was added.`,
      at: f.at,
      fact: { kind: f.kind, key: f.key, help: f.help },
    });
  }
  for (const a of s.attention ?? []) {
    if (a.asks) {
      // One row per question you can answer; opening it opens the card in
      // its chat (nothing is answered from the Inbox).
      for (const q of a.asks) {
        const row = { askId: q.askId, chatId: a.sessionId, chatTitle: titleOf(a.sessionId) };
        if (q.eligible) out.push({ key: `ask:${q.askId}`, kind: "ask", title: `${q.agentName} asks you`, count: 1, ...row });
        else waitingOnOthers.push({ key: `waiting:${q.askId}`, kind: "waiting", title: `${q.agentName} asks ${q.audience}`, ...row });
      }
    } else if (a.openAsks > 0) {
      out.push({
        key: `ask:${a.sessionId}`,
        kind: "ask",
        title: a.openAsks === 1 ? "An agent asked you something" : `Agents asked you ${a.openAsks} questions`,
        chatId: a.sessionId,
        chatTitle: titleOf(a.sessionId),
        count: a.openAsks,
      });
    }
    for (const p of a.proposals) {
      if (!p.needsMe) continue;
      const apply = p.status === "approved";
      out.push({
        key: `${apply ? "apply" : "vote"}:${p.id}`,
        kind: apply ? "apply" : "vote",
        title: apply ? `Ready to apply: ${p.title || "a change"}` : `Your vote: ${p.title || "a change"}`,
        chatId: a.sessionId,
        chatTitle: titleOf(a.sessionId),
        proposalId: p.id,
      });
    }
  }
  for (const [chatId, l] of s.live ?? []) {
    if (!l.failed) continue;
    out.push({ key: `failed:${chatId}`, kind: "failed", title: "The last run failed", chatId, chatTitle: titleOf(chatId) });
  }
  if (s.sandboxChanged) {
    out.push({ key: "sandbox", kind: "sandbox", title: "This workspace's sandbox grants changed" });
  }
  if ((s.trustWaiting ?? 0) > 0) {
    const n = s.trustWaiting!;
    out.push({ key: "trust", kind: "trust", title: `This workspace wants to run ${n} ${plural(n, "command", "commands")}` });
  }
  for (const r of s.deviceRequests ?? []) {
    out.push({ key: `device:${r.hostId}`, kind: "device", title: `${r.label || "A box"} says it's your device` });
  }
  for (const i of s.invites ?? []) {
    out.push({
      key: `invite:${i.relay}#${i.seq}`,
      kind: "invite",
      title: i.isDm
        ? `${i.founderName || "Someone"} wants to message you`
        : `Invite to join ${i.workspaceName || "a workspace"}`,
    });
  }
  // Questions put to others show only when nothing else waits on you.
  if (out.length === 0) out.push(...waitingOnOthers);
  return out.sort((a, b) => ORDER[a.kind] - ORDER[b.kind]);
}

/// The badge number: one per item, except an "ask" row counts each question,
/// a collapsed approval row each request, and facts nothing.
export function needsYouCount(items: readonly NeedsYouItem[]): number {
  return items.reduce((n, i) => {
    if (i.kind === "approvalFact" || i.kind === "waiting") return n;
    if (i.kind === "ask") return n + (i.count ?? 1);
    if (i.kind === "approval") return n + (i.approvals?.length ?? 1);
    return n + 1;
  }, 0);
}

/// "3 need you" / "1 needs you".
export function needsYouLabel(n: number): string {
  return `${n} ${n === 1 ? "needs" : "need"} you`;
}

/// The Review pane's badge for one chat: proposals waiting on your vote or
/// ready to apply. Undefined (no badge) when there are none.
export function reviewBadge(attention: readonly ChatAttention[] | undefined, chatId: string | null): number | undefined {
  if (!chatId) return undefined;
  const n = (attention ?? []).find((a) => a.sessionId === chatId)?.proposals.filter((p) => p.needsMe).length ?? 0;
  return n > 0 ? n : undefined;
}
