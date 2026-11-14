// Fixtures for proposal-set view tests.
import type { ProposalSetDto } from "@/bindings/ProposalSetDto";
import type { SetAlternativeDto } from "@/bindings/SetAlternativeDto";

export function alt(over: Partial<SetAlternativeDto> & { proposalId: string }): SetAlternativeDto {
  return {
    label: "A",
    title: over.proposalId,
    body: "",
    kind: "decision",
    status: "open",
    changedFiles: [],
    support: 0,
    qualifyingScouts: 0,
    scouts: 0,
    runtimes: 0,
    meetsQuorum: false,
    frozen: false,
    active: true,
    superseded: false,
    stopReason: null,
    stoppedBy: null,
    assessments: [],
    pendingScouts: [],
    qualifyingApprovals: 0,
    requiredApprovals: 1,
    preflight: null,
    reverted: false,
    ...over,
  };
}

export function set(over: Partial<ProposalSetDto>): ProposalSetDto {
  return {
    id: "s1",
    question: "Which cache?",
    body: "",
    author: "planner",
    taskClass: null,
    phase: "scouting",
    round: 0,
    attempt: 0,
    agentQuorum: 2,
    qualityBar: 70,
    decayPerRound: 25,
    requiredApprovals: 1,
    approvalRoleFloor: "contributor",
    dial: "balanced",
    scoutingDeadline: null,
    deadlinePassed: false,
    frontRunner: null,
    tieBreak: false,
    winner: null,
    decidedAt: null,
    alternatives: [],
    createdAt: "2026-11-13T10:00:00Z",
    ...over,
  };
}
