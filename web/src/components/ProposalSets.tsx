// The proposal-set view in the Review pane: competing alternatives, each scored
// blind by agent scouts, with quorum state, stop signals, the "fast ↔ careful"
// dial, and the human two-phase commit (approve → pre-flight → apply).
//
// Scores appear only once a scout has posted one; a scout that's been sent but
// hasn't answered shows as "awaiting", never as a number.

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  advanceProposalSetRound,
  applyProposalSet,
  closeProposalSet,
  listProposalSets,
  preflightProposalSet,
  revertProposal,
  sendScouts,
  tuneProposalSet,
  voteProposal,
  withdrawSetAlternative,
  type ProposalSetDto,
  type SetAlternativeDto,
} from "@/lib/ipc";
import type { PreflightCheckDto } from "@/bindings/PreflightCheckDto";
import { DIALS, isOpen, nextStep, phaseLine, supportText } from "@/lib/proposalSets";
import { relTime } from "@/lib/time";
import { Button } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

const TONE = {
  ok: "var(--hive-success)",
  bad: "var(--hive-danger)",
  warn: "var(--hive-warn)",
  ink: "var(--hive-ink)",
  accent: "var(--hive-accent-cool)",
};

function Chip({ tone, children }: { tone: string; children: React.ReactNode }) {
  return (
    <span
      className="rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.1em]"
      style={{ color: tone, background: `color-mix(in srgb, ${tone} 14%, transparent)` }}
    >
      {children}
    </span>
  );
}

/// The sets for a chat, for the Review pane. Polls while a set is open so
/// scout results and the deadline tie-break show up without a refresh.
export function useProposalSets(sessionId: string) {
  return useQuery({
    queryKey: ["proposal-sets", sessionId],
    queryFn: () => listProposalSets(sessionId),
    refetchInterval: (q) => ((q.state.data ?? []).some(isOpen) ? 5000 : false),
  });
}

export function ProposalSets({ sessionId, sets }: { sessionId: string; sets: ProposalSetDto[] }) {
  if (sets.length === 0) return null;
  return (
    <div className="space-y-2.5">
      {sets.map((s) => (
        <ProposalSetCard key={s.id} sessionId={sessionId} set={s} />
      ))}
    </div>
  );
}

function ProposalSetCard({ sessionId, set }: { sessionId: string; set: ProposalSetDto }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(isOpen(set));
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["proposal-sets", sessionId] });
    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    qc.invalidateQueries({ queryKey: ["chat", sessionId] });
  };
  const act = useMutation({
    mutationFn: (fn: () => Promise<unknown>) => fn(),
    onSuccess: refresh,
    onError: (e) => {
      toast.error(errMsg(e));
      refresh();
    },
  });
  const busy = act.isPending;
  const run = (fn: () => Promise<unknown>, ok?: string) =>
    act.mutate(async () => {
      const r = await fn();
      if (ok) toast.success(ok);
      return r;
    });
  const live = isOpen(set);
  const maxSupport = Math.max(1, ...set.alternatives.map((a) => a.support));

  return (
    <div
      className="rounded-2xl border p-3"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
      data-testid="proposal-set"
    >
      <button className="flex w-full items-start gap-2 text-left" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        <span aria-hidden className="mt-0.5 text-xs opacity-60">
          {open ? "▾" : "▸"}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block font-semibold">{set.question}</span>
          <span className="mt-0.5 block text-xs opacity-60">{phaseLine(set)}</span>
        </span>
        <Chip tone={set.phase === "piping" ? TONE.accent : set.phase === "decided" ? TONE.ok : TONE.ink}>
          {set.phase}
        </Chip>
      </button>

      {open && (
        <div className="mt-3 space-y-3 pl-[18px]">
          <div className="flex flex-wrap items-center gap-1.5 text-xs opacity-70">
            {set.taskClass && <Chip tone={TONE.ink}>{set.taskClass}</Chip>}
            <span>Round {set.round + 1}</span>
            <span>· support decays {set.decayPerRound}/round</span>
            {set.scoutingDeadline && set.phase === "scouting" && (
              <span>· tie-break {set.deadlinePassed ? "due" : relTime(set.scoutingDeadline)}</span>
            )}
          </div>

          <QuorumDial
            set={set}
            disabled={!live || busy}
            onDial={(dial) => run(() => tuneProposalSet(sessionId, set.id, { dial }))}
          />

          <div className="space-y-2">
            {set.alternatives.map((alt) => (
              <Alternative
                key={alt.proposalId}
                set={set}
                alt={alt}
                maxSupport={maxSupport}
                busy={busy}
                onVote={(approved) => run(() => voteProposal(sessionId, alt.proposalId, approved))}
                onPreflight={() => run(() => preflightProposalSet(sessionId, set.id, alt.proposalId))}
                onApply={() => run(() => applyProposalSet(sessionId, set.id, alt.proposalId), "Applied.")}
                onWithdraw={() => run(() => withdrawSetAlternative(sessionId, set.id, alt.proposalId))}
                onRevert={() => run(() => revertProposal(sessionId, alt.proposalId), "Reverted.")}
              />
            ))}
          </div>

          {live && (
            <div className="flex flex-wrap gap-2">
              {set.phase === "scouting" && (
                <Button
                  variant="primary"
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      const n = await sendScouts(sessionId, set.id);
                      toast.success(`Sent ${n} scout${n === 1 ? "" : "s"}.`);
                    })
                  }
                >
                  Send scouts
                </Button>
              )}
              <Button
                disabled={busy}
                title="New evidence: older scout support decays one round"
                onClick={() => run(() => advanceProposalSetRound(sessionId, set.id))}
              >
                New round
              </Button>
              <Button className="ml-auto" disabled={busy} onClick={() => run(() => closeProposalSet(sessionId, set.id))}>
                Close without choosing
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function QuorumDial({
  set,
  disabled,
  onDial,
}: {
  set: ProposalSetDto;
  disabled: boolean;
  onDial: (dial: string) => void;
}) {
  return (
    <div>
      <div className="mb-1 flex items-center justify-between text-xs">
        <span className="opacity-70">Quorum</span>
        <span className="opacity-60">
          {set.agentQuorum} scout{set.agentQuorum === 1 ? "" : "s"} ≥ {set.qualityBar} · {set.requiredApprovals} human
          approval{set.requiredApprovals === 1 ? "" : "s"} ({set.approvalRoleFloor}+)
        </span>
      </div>
      <div
        role="radiogroup"
        aria-label="Fast to careful"
        className="flex overflow-hidden rounded-xl border text-xs"
        style={{ borderColor: "var(--hive-line)" }}
      >
        {DIALS.map((d) => {
          const on = set.dial === d.id;
          return (
            <button
              key={d.id}
              role="radio"
              aria-checked={on}
              title={d.hint}
              disabled={disabled}
              onClick={() => !on && onDial(d.id)}
              className="flex-1 px-2 py-1.5 font-medium transition-colors disabled:cursor-not-allowed"
              style={{
                background: on ? "color-mix(in srgb, var(--hive-accent-cool) 22%, transparent)" : "var(--hive-mist)",
                color: "var(--hive-ink)",
                opacity: disabled && !on ? 0.5 : 1,
              }}
            >
              {d.label}
            </button>
          );
        })}
      </div>
      {set.dial === "custom" && <div className="mt-1 text-[11px] opacity-60">Custom knobs</div>}
    </div>
  );
}

function Alternative({
  set,
  alt,
  maxSupport,
  busy,
  onVote,
  onPreflight,
  onApply,
  onWithdraw,
  onRevert,
}: {
  set: ProposalSetDto;
  alt: SetAlternativeDto;
  maxSupport: number;
  busy: boolean;
  onVote: (approved: boolean) => void;
  onPreflight: () => void;
  onApply: () => void;
  onWithdraw: () => void;
  onRevert: () => void;
}) {
  const [showWhy, setShowWhy] = useState(false);
  const front = set.frontRunner === alt.proposalId;
  const winner = set.winner === alt.proposalId;
  const step = nextStep(set, alt);
  const awaiting = Math.max(0, set.agentQuorum - alt.scouts - alt.pendingScouts.length);
  return (
    <div
      className="rounded-xl border p-2.5"
      data-testid={`alternative-${alt.label}`}
      style={{
        borderColor: front || winner ? "var(--hive-accent-cool)" : "var(--hive-line)",
        opacity: alt.active || winner ? 1 : 0.6,
      }}
    >
      <div className="flex items-start gap-2">
        <span className="font-mono text-xs font-semibold opacity-60">{alt.label}</span>
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">{alt.title}</div>
          {alt.body && <p className="mt-0.5 line-clamp-3 text-xs leading-5 opacity-70">{alt.body}</p>}
          {alt.changedFiles.length > 0 && (
            <div className="mt-0.5 text-[11px] opacity-50">
              {alt.changedFiles.length} file{alt.changedFiles.length === 1 ? "" : "s"} changed
            </div>
          )}
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1">
          {winner && <Chip tone={TONE.ok}>winner</Chip>}
          {front && !winner && <Chip tone={TONE.accent}>front-runner</Chip>}
          {alt.stopReason && <Chip tone={TONE.bad}>{alt.stopReason === "withdrawn" ? "withdrawn" : "stopped"}</Chip>}
          {alt.frozen && !alt.stopReason && !winner && set.phase !== "decided" && <Chip tone={TONE.warn}>frozen</Chip>}
          {alt.reverted && <Chip tone={TONE.warn}>reverted</Chip>}
        </div>
      </div>

      {/* Support: decayed agent support, bar relative to the leader. */}
      <div className="mt-2">
        <div className="h-1.5 overflow-hidden rounded-full" style={{ background: "var(--hive-mist)" }}>
          <div
            className="h-full rounded-full"
            style={{
              width: `${Math.round((alt.support / maxSupport) * 100)}%`,
              background: alt.meetsQuorum ? TONE.ok : TONE.accent,
            }}
          />
        </div>
        <div className="mt-1 flex flex-wrap gap-x-2 text-[11px] opacity-70">
          <span>
            {alt.qualifyingScouts}/{set.agentQuorum} scouts over the bar
          </span>
          <span>· support {alt.support}</span>
          {alt.scouts > 0 && (
            <span>
              · {alt.runtimes} runtime{alt.runtimes === 1 ? "" : "s"}
              {alt.scouts > 1 && alt.runtimes === 1 ? " (same model: correlated)" : ""}
            </span>
          )}
        </div>
      </div>

      {/* Posted assessments only; pending scouts never show a number. */}
      {(alt.assessments.length > 0 || alt.pendingScouts.length > 0 || (awaiting > 0 && alt.active && set.phase === "scouting")) && (
        <div className="mt-2 space-y-1">
          {alt.assessments.map((a) => (
            <div key={a.id} className="flex items-center gap-1.5 text-xs" data-testid="assessment">
              <span className="font-medium">{a.scoutName}</span>
              {a.scoutRuntime && <span className="opacity-50">{a.scoutRuntime}</span>}
              <span className="ml-auto font-mono">{supportText(a.score, a.currentSupport)}</span>
              {a.isSelf && <span className="opacity-60">(own; not counted)</span>}
            </div>
          ))}
          {alt.pendingScouts.map((n) => (
            <div key={n} className="flex items-center gap-1.5 text-xs opacity-60" data-testid="pending-scout">
              <span className="font-medium">{n}</span>
              <span className="ml-auto italic">assessing…</span>
            </div>
          ))}
          {set.phase === "scouting" && alt.active && awaiting > 0 && (
            <div className="text-[11px] italic opacity-50">
              {awaiting} more scout{awaiting === 1 ? "" : "s"} needed
            </div>
          )}
          {alt.assessments.some((a) => a.rationale) && (
            <button className="text-[11px] underline opacity-60" onClick={() => setShowWhy((v) => !v)}>
              {showWhy ? "Hide rationale" : "Why?"}
            </button>
          )}
          {showWhy &&
            alt.assessments
              .filter((a) => a.rationale)
              .map((a) => (
                <p key={a.id} className="text-xs leading-5 opacity-75">
                  <span className="font-medium">{a.scoutName}:</span> {a.rationale}
                </p>
              ))}
        </div>
      )}

      {alt.preflight && <Preflight checks={alt.preflight} />}

      {/* The human gate: approvals never decay. */}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <span className="text-[11px] opacity-60">
          {alt.qualifyingApprovals}/{alt.requiredApprovals} approval{alt.requiredApprovals === 1 ? "" : "s"}
        </span>
        {step === "approve" && (
          <>
            <Button size="sm" disabled={busy} onClick={() => onVote(true)}>
              Approve
            </Button>
            <Button size="sm" variant="danger" disabled={busy} onClick={() => onVote(false)}>
              Reject
            </Button>
          </>
        )}
        {step === "preflight" && (
          <Button size="sm" variant="primary" disabled={busy} onClick={onPreflight}>
            {alt.preflight ? "Re-run pre-flight" : "Run pre-flight"}
          </Button>
        )}
        {step === "apply" && (
          <Button size="sm" variant="primary" disabled={busy} onClick={onApply}>
            Apply
          </Button>
        )}
        {step === "revert" && (
          <Button size="sm" variant="danger" disabled={busy} onClick={onRevert}>
            Revert
          </Button>
        )}
        {isOpen(set) && alt.active && !front && (
          <button className="ml-auto text-[11px] opacity-60 hover:opacity-100" disabled={busy} onClick={onWithdraw}>
            Withdraw
          </button>
        )}
      </div>
    </div>
  );
}

function Preflight({ checks }: { checks: NonNullable<SetAlternativeDto["preflight"]> }) {
  const row = (name: string, c: PreflightCheckDto) => (
    <div key={name} className="flex items-center gap-1.5 text-xs">
      <span
        aria-hidden
        style={{ color: c.status === "passed" ? TONE.ok : c.status === "skipped" ? TONE.ink : TONE.bad }}
      >
        {c.status === "passed" ? "✓" : c.status === "skipped" ? "–" : "✕"}
      </span>
      <span className="font-medium">{name}</span>
      <span className="truncate opacity-60">{c.note}</span>
    </div>
  );
  return (
    <div className="mt-2 rounded-lg p-2" style={{ background: "var(--hive-mist)" }} data-testid="preflight">
      <div className="mb-1 text-[11px] font-semibold" style={{ color: checks.passed ? TONE.ok : TONE.bad }}>
        Pre-flight {checks.passed ? "passed" : "failed"}
      </div>
      {row("Tests", checks.tests)}
      {row("Base unchanged", checks.baseUnchanged)}
      {row("Hosts online", checks.hostsOnline)}
    </div>
  );
}
