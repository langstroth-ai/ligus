// Workspace memory and live task state. Typed wrappers around the
// `memory_*` and `task_state` Tauri commands (app/src/memory_ui.rs); the
// shapes mirror the serde (camelCase) structs in hive_core::memory.
import { invoke } from "@tauri-apps/api/core";

export type ActorKind = "human" | "assistant" | "agent" | "system" | "service";

export interface MemoryAuthor {
  id: string;
  name: string;
  kind: ActorKind;
}

/// A chat's sandbox egress (hive_core::sandbox::EgressPolicy).
export type EgressPolicy = { mode: "open" } | { mode: "allowList"; mcpServers: string[] } | { mode: "closed" };

export interface MemorySource {
  chatId?: string;
  messageId?: string;
  turnId?: string;
  /// The source chat's egress when the note was written (left out when open).
  egress?: EgressPolicy;
}

export interface MemoryRevision {
  rev: string;
  base?: string;
  title: string;
  body: string;
  tags?: string[];
  author: MemoryAuthor;
  atMs: number;
  source?: MemorySource;
  viaProposal?: string;
  proposer?: string;
  removal?: boolean;
}

export interface MemoryEntry {
  id: string;
  current: MemoryRevision;
  createdBy: MemoryAuthor;
  createdAtMs: number;
  revisions?: string[];
  revisionCount: number;
  conflicts?: MemoryRevision[];
  removed?: MemoryRevision;
  /// Edits that sorted after the removal: who and when, never their text.
  editedAfterRemoval?: MemoryRevision[];
  /// The strictest egress it was written under (absent: the workspace default).
  egress?: EgressPolicy;
}

export type MemoryOp =
  | { op: "create"; entryId: string; title: string; body: string; tags?: string[] }
  | { op: "edit"; entryId: string; base: string; title: string; body: string; tags?: string[] }
  | { op: "remove"; entryId: string; base: string };

export interface MemoryProposalState {
  proposal: { proposalId: string; op: MemoryOp; proposer?: string; source?: MemorySource; rationale?: string };
  by: MemoryAuthor;
  atMs: number;
  decision?: { accepted: boolean; by: MemoryAuthor; atMs: number; note?: string; rev: string; conflicted?: boolean };
}

export interface MemoryView {
  entries: MemoryEntry[];
  total: number;
  proposals: MemoryProposalState[];
  chatTitles: Record<string, string>;
  /// Agent keys (an agent id, or "primary" for the chat's runtime) → names.
  agentNames: Record<string, string>;
  me: string;
  moderator: boolean;
  /// Removed notes someone edited after the removal (not applied; shown so
  /// the edit isn't lost silently).
  editedAfterRemoval?: MemoryEntry[];
}

export type TaskStatus = "working" | "blocked" | "waiting" | "done" | "abandoned";

export interface TaskSlotDto {
  update: {
    taskId: string;
    agentKey: string;
    turnId: string;
    status: TaskStatus;
    summary: string;
    progress?: number;
    openQuestions?: string[];
    partialResults?: string[];
    ttlMs?: number;
  };
  writer: string;
  by: MemoryAuthor;
  atMs: number;
  rev: string;
  updates: number;
  agentName: string;
  active: boolean;
  stale: boolean;
  expiresAtMs: number;
}

/// Bounds the fold enforces (hive_core::memory), checked here so a form
/// can say what's wrong before signing.
export const MAX_TITLE = 120;
export const MAX_BODY = 4000;
export const MAX_TAGS = 8;

export const memoryView = (query: string, tags: string[]) => invoke<MemoryView>("memory_view", { query, tags });

export const writeMemory = (v: {
  entryId?: string;
  base?: string;
  title: string;
  body: string;
  tags: string[];
  resolves?: string[];
  sourceChatId?: string;
}) =>
  invoke<{ entryId: string; rev: string }>("memory_write", {
    entryId: v.entryId ?? null,
    base: v.base ?? null,
    title: v.title,
    body: v.body,
    tags: v.tags,
    resolves: v.resolves ?? [],
    sourceChatId: v.sourceChatId ?? null,
  });

export const removeMemory = (entryId: string, base: string, resolves: string[] = []) =>
  invoke<void>("memory_remove", { entryId, base, resolves });

export const decideMemoryProposal = (proposalId: string, accepted: boolean, note = "") =>
  invoke<void>("memory_decide", { proposalId, accepted, note });

/// Propose a change as this device's person (for a note they can't change
/// directly: someone else's). A person with the right decides it.
export const proposeMemory = (op: MemoryOp, rationale = "") =>
  invoke<string>("memory_propose", {
    op: op.op,
    entryId: op.op === "create" ? null : op.entryId,
    base: op.op === "create" ? null : op.base,
    title: op.op === "remove" ? null : op.title,
    body: op.op === "remove" ? null : op.body,
    tags: op.op === "remove" ? null : (op.tags ?? []),
    rationale,
  });

export const taskState = (sessionId: string) => invoke<TaskSlotDto[]>("task_state", { sessionId });

/// Split a comma or space separated tag field into normalised tags.
export function parseTags(s: string): string[] {
  const out: string[] = [];
  for (const raw of s.split(/[,\s]+/)) {
    const t = raw
      .toLowerCase()
      .replace(/[^a-z0-9\-_.:]/g, "")
      .slice(0, 32);
    if (t && !out.includes(t)) out.push(t);
  }
  return out.slice(0, MAX_TAGS);
}

/// Why a draft can't be saved, or null.
export function draftProblem(title: string, body: string): string | null {
  if (!title.trim()) return "A title is required.";
  if (/[\n\r]/.test(title)) return "The title is one line.";
  if ([...title.trim()].length > MAX_TITLE) return `The title is at most ${MAX_TITLE} characters.`;
  if (!body.trim()) return "The note is empty.";
  if ([...body].length > MAX_BODY) return `A note is at most ${MAX_BODY} characters.`;
  return null;
}

/// Who wrote a revision, in words ("Ana", "Ana, proposed by an agent").
export function provenance(r: MemoryRevision, agentName?: (key: string) => string | undefined): string {
  const who = r.author.name || r.author.id;
  if (r.proposer) {
    const agent = agentName?.(r.proposer) ?? (r.proposer === "primary" ? "Ligus" : "an agent");
    return `${who}, proposed by ${agent}`;
  }
  return who;
}

/// Who filed a proposal, in words. A proposal naming an agent is that agent's,
/// whether its own key signed it or its host did ("coder (an agent), on
/// Ana's device"); never shown as only the host's person.
export function proposedBy(p: MemoryProposalState, agentNames: Record<string, string> = {}): string {
  const proposer = p.proposal.proposer;
  const signer = p.by.name || p.by.id;
  if (!proposer) return signer;
  const agent = agentNames[proposer] ?? (proposer === "primary" ? "Ligus" : "an agent");
  return p.by.kind === "human" ? `${agent} (an agent), on ${signer}'s device` : `${agent} (an agent)`;
}

/// Which agent turns read a note, from the egress it was written under, or
/// null when every turn does (the workspace default).
export function egressNote(e: MemoryEntry): string | null {
  switch (e.egress?.mode) {
    case "closed":
      return "Written from a chat with egress closed: only agent turns with egress closed read it.";
    case "allowList":
      return `Written from a chat with restricted egress: only agent turns limited to ${e.egress.mcpServers.length ? e.egress.mcpServers.join(", ") : "no servers"} (or stricter) read it.`;
    default:
      return null;
  }
}

/// Whether this person may change `e` directly: its creator or a moderator.
export const canChange = (e: MemoryEntry, me: string, moderator: boolean) => moderator || e.createdBy.id === me;

export const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  working: "Working",
  blocked: "Blocked",
  waiting: "Waiting",
  done: "Done",
  abandoned: "Stopped",
};
