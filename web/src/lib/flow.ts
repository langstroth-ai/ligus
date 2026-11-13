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
