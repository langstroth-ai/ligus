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
  /// The open repository ships commands that stay inert until you enable them.
  | "trust"
  /// The workspace's sandbox grants changed since you last looked.
  | "sandbox";

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
}

/// Most urgent first: something that blocks an agent or a security check,
/// then decisions, then failures, then requests from outside.
const ORDER: Record<NeedsYouKind, number> = {
  phone: 0,
  ask: 1,
  vote: 2,
  apply: 3,
  failed: 4,
  sandbox: 5,
  trust: 6,
  device: 7,
  invite: 8,
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
}

function plural(n: number, one: string, many: string) {
  return n === 1 ? one : many;
}

/// Everything waiting on this person, most urgent first.
export function needsYouItems(s: NeedsYouSources): NeedsYouItem[] {
  const out: NeedsYouItem[] = [];
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
  for (const a of s.attention ?? []) {
    if (a.openAsks > 0) {
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
  return out.sort((a, b) => ORDER[a.kind] - ORDER[b.kind]);
}

/// The badge number: one per item, except an "ask" row counts each question.
export function needsYouCount(items: readonly NeedsYouItem[]): number {
  return items.reduce((n, i) => n + (i.kind === "ask" ? (i.count ?? 1) : 1), 0);
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
