// Pure helpers for the proposal-set view, factored out so the rules are
// unit-testable without rendering the pane.

import type { ProposalDto } from "@/bindings/ProposalDto";
import type { ProposalSetDto } from "@/bindings/ProposalSetDto";
import type { SetAlternativeDto } from "@/bindings/SetAlternativeDto";

export const DIALS = [
  { id: "fast", label: "Fast", hint: "1 scout over 60, 1 approval" },
  { id: "balanced", label: "Balanced", hint: "2 scouts over 70, 1 approval" },
  { id: "careful", label: "Careful", hint: "3 scouts over 80, 2 approvals" },
] as const;

/// Proposals that aren't alternatives of a set: the plain Review inbox. Set
/// alternatives render inside their set instead.
export function withoutSetAlternatives(proposals: readonly ProposalDto[], sets: readonly ProposalSetDto[]): ProposalDto[] {
  const inSet = new Set(sets.flatMap((s) => s.alternatives.map((a) => a.proposalId)));
  return proposals.filter((p) => !inSet.has(p.id) && !(p.setId && sets.some((s) => s.id === p.setId)));
}

export function isOpen(set: ProposalSetDto): boolean {
  return set.phase === "scouting" || set.phase === "piping";
}

/// The one next step a person can take on an alternative.
export type NextStep = "approve" | "preflight" | "apply" | "revert" | null;

export function nextStep(set: ProposalSetDto, alt: SetAlternativeDto): NextStep {
  if (set.phase === "decided") {
    return set.winner === alt.proposalId && alt.status === "applied" && alt.kind === "fileDiff" && !alt.reverted
      ? "revert"
      : null;
  }
  if (!isOpen(set) || !alt.active) return null;
  if (alt.status !== "approved") return "approve";
  if (!alt.preflight || !alt.preflight.passed) return "preflight";
  return "apply";
}

/// A one-line summary of where the set is.
export function phaseLine(set: ProposalSetDto): string {
  const front = set.alternatives.find((a) => a.proposalId === set.frontRunner);
  const winner = set.alternatives.find((a) => a.proposalId === set.winner);
  switch (set.phase) {
    case "scouting":
      return set.attempt > 0
        ? `Scouting again (attempt ${set.attempt + 1}): the last pre-flight failed.`
        : `Scouting: ${set.agentQuorum} independent scout${set.agentQuorum === 1 ? "" : "s"} at ${set.qualityBar}+ reach quorum.`;
    case "piping":
      return front
        ? `${front.label} is the front-runner${set.tieBreak ? " (tie-break at the deadline)" : ""}. Rivals are frozen; your call.`
        : "Waiting for a decision.";
    case "decided":
      return winner ? `Decided: ${winner.label} was applied.` : "Closed without choosing.";
    default:
      return "This set was changed by a newer version of Ligus.";
  }
}

/// A scout's support as shown: the posted score, and what's left of it after decay.
export function supportText(score: number, current: number): string {
  return current === score ? `${score}` : `${score} → ${current}`;
}
