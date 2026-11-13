// Flow: routing, hand-offs, backpressure, the task board and workspace marks.
// Typed wrappers around the `flow_*` Tauri commands (app/src/flow.rs). The
// shapes mirror the serde (camelCase) structs in hive_core::flow.
import { invoke } from "@tauri-apps/api/core";

export interface HandoffQuality {
  testsPassed?: boolean;
  proposalId?: string;
  proposalStatus?: string;
  turns: number;
  costUsd: number;
  error: boolean;
}

export interface ContextParcel {
  keyFiles?: string[];
  decisions?: string[];
  ruledOut?: string[];
  notes?: string;
}

export interface HandoffCard {
  id: string;
  from: string;
  to: string;
  fromName: string;
  toName: string;
  messageId?: string;
  task: string;
  taskClass: string;
  branch?: string;
  files?: string[];
  quality: HandoffQuality;
  via: string;
  parcel?: ContextParcel;
  atMs: number;
}

export type RouteMode = "off" | "shadow" | "live";
export type StopUpstream = "off" | "on" | "ab";

export interface FlowSettings {
  handoffCards: boolean;
  routeMode: RouteMode;
  epsilonMin: number;
  epsilonMax: number;
  halfLifeDays: number;
  cooldownMinutes: number;
  stopUpstream: StopUpstream;
  queueLimit: number;
  longWaitMinutes: number;
  recruitApprovers: boolean;
  offerIdleHost: boolean;
  marksInContext: boolean;
  boardAutoPickup: boolean;
  reuseWorktrees: boolean;
}

export const getFlowSettings = () => invoke<FlowSettings>("flow_get_settings");
export const setFlowSettings = (settings: FlowSettings) => invoke<FlowSettings>("flow_set_settings", { settings });

export const listHandoffs = (sessionId: string) => invoke<HandoffCard[]>("flow_handoffs", { sessionId });

/// Index cards by the message that handed the work on, for the transcript.
export function handoffsByMessage(cards: HandoffCard[]): Map<string, HandoffCard[]> {
  const out = new Map<string, HandoffCard[]>();
  for (const c of cards) {
    if (!c.messageId) continue;
    const list = out.get(c.messageId) ?? [];
    list.push(c);
    out.set(c.messageId, list);
  }
  return out;
}

/// One-line "how good" summary of a card's measured quality.
export function qualityLine(q: HandoffQuality): string {
  const parts: string[] = [];
  if (q.testsPassed === true) parts.push("tests passed");
  if (q.testsPassed === false) parts.push("tests failing");
  if (q.proposalStatus) parts.push(`proposal ${q.proposalStatus}`);
  if (q.error) parts.push("previous turn errored");
  if (q.turns > 0) parts.push(`${q.turns} turn${q.turns === 1 ? "" : "s"} so far`);
  return parts.join(" · ");
}

export type RouteReason = "sampled" | "explore" | "threadMemory" | "fileMemory" | "only" | "fallback";

export interface ScoreRow {
  agent: string;
  agentName: string;
  taskClass: string;
  score: number;
  recent: number;
  weight: number;
  outcomes: number;
  successes: number;
  turnsPerSuccess: number | null;
}

export interface DecisionRow {
  atMs: number;
  sessionId: string;
  taskClass: string;
  mode: RouteMode;
  chosen: string;
  wouldPick: string;
  preferred: string;
  explore: boolean;
  reason: RouteReason;
  epsilon: number;
}

export interface ArmRow {
  arm: string;
  turns: number;
  proposals: number;
  accepted: number;
  judged: number;
  acceptanceRate: number | null;
  turnsPerAccepted: number | null;
  costUsd: number;
  costPerAccepted: number | null;
  localShare: number;
}

export interface RoutingReport {
  mode: RouteMode;
  sinceMs: number;
  decisions: number;
  offPrimaryShare: number;
  scores: ScoreRow[];
  arms: ArmRow[];
  recent: DecisionRow[];
  stops: StopRow[];
  exploration: ExplorationRow;
}

export interface ExplorationRow {
  explored: number;
  share: number;
  meanEpsilon: number;
  exploredWins: number;
  exploredJudged: number;
  incumbentRate: number | null;
}

export interface StopRow {
  route: string;
  name: string;
  reason: string;
  untilMs: number;
  failures: number;
}

export const clearRouteStop = (route: string) => invoke<void>("flow_clear_route_stop", { route });

export function minutesLeft(untilMs: number, now = Date.now()): number {
  return Math.max(0, Math.ceil((untilMs - now) / 60_000));
}

export const routingReport = (days?: number) => invoke<RoutingReport>("flow_routing_report", { days: days ?? null });

export const pct = (x: number | null | undefined) => (x == null ? "—" : `${Math.round(x * 100)}%`);

export const REASON_LABEL: Record<RouteReason, string> = {
  sampled: "sampled by score",
  explore: "exploring",
  threadMemory: "kept the thread",
  fileMemory: "knows the file",
  only: "only candidate",
  fallback: "all stopped → primary",
};

export interface GateQueue {
  open: number;
  oldestWaitMs: number;
  medianWaitMs: number;
  trembleShare: number;
  openedLastHour: number;
  decidedLastHour: number;
  overLimit: boolean;
}

export interface EdgeStats {
  edge: "gate" | "stage" | "handoff";
  samples: number;
  medianWaitMs: number;
  trembleShare: number;
}

export interface RecruitNotice {
  id: string;
  action: "approvers" | "idle_host" | "stop_upstream" | string;
  queueLen: number;
  oldestWaitMs: number;
  memberIds?: string[];
  toHost?: string;
  fromHost?: string;
  reference?: string;
  atMs: number;
}

export interface QueueReport {
  gate: GateQueue;
  edges: EdgeStats[];
  alert: string | null;
  queueLimit: number;
  stopUpstream: StopUpstream;
  recruits: RecruitNotice[];
}

export const queueReport = () => invoke<QueueReport>("flow_queue_report");

export function duration(ms: number): string {
  const m = Math.round(ms / 60_000);
  if (m < 60) return `${m} min`;
  const h = Math.round(m / 60);
  return h < 48 ? `${h} h` : `${Math.round(h / 24)} d`;
}

export const RECRUIT_LABEL: Record<string, string> = {
  approvers: "asked more reviewers",
  idle_host: "an idle host took a queued stage",
  stop_upstream: "told agents to stop opening new proposals",
};

export type MarkKind = "claim" | "underConstruction" | "needsReview";

export interface MarkRow {
  id: string;
  kind: MarkKind;
  path?: string;
  lines?: [number, number];
  taskId?: string;
  note: string;
  agent: string;
  by: string;
  sessionId?: string;
  atMs: number;
  expiresMs?: number;
  chatTitle: string;
}

export const listMarks = () => invoke<MarkRow[]>("flow_marks");
export const addMark = (kind: "claim" | "lock" | "review", path: string, note: string, sessionId?: string) =>
  invoke<void>("flow_add_mark", { kind, path, note, sessionId: sessionId ?? null });
export const clearMark = (markId: string) => invoke<void>("flow_clear_mark", { markId });

export const MARK_LABEL: Record<MarkKind, string> = {
  claim: "Claim",
  underConstruction: "Under construction",
  needsReview: "Needs review",
};
