// The proposal-set view in the Review pane: competing alternatives, each scored
// blind by agent scouts, with quorum state, stop signals, the "fast ↔ careful"
// dial, and the human two-phase commit (approve → pre-flight → apply).
//
// Scores appear only once a scout has posted one; a scout that's been sent but
// hasn't answered shows as "awaiting", never as a number.

import { Markdown } from "@/components/Markdown";
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  advanceProposalSetRound,
  applyProposalSet,
  closeProposalSet,
  listProposalSets,
  listProposals,
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
import { Badge, Button, type Tone } from "@/components/ui";
import { ProposalDiffModal } from "@/components/RightRail";
import { IconCheck, IconChevronRight, IconX } from "@/lib/icons";
import type { ProposalDto } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";

function Chip({ tone, icon, children }: { tone: Tone; icon?: React.ReactNode; children: React.ReactNode }) {
  return (
    <Badge tone={tone} icon={icon}>
      {children}
    </Badge>
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
      className="rounded-xl border"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
      data-testid="proposal-set"
    >
      <button
        className="flex w-full items-start gap-2 rounded-xl px-3 py-2.5 text-left transition-colors hover:bg-[color:var(--ligus-hover)]"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span
          aria-hidden
          className="mt-0.5 inline-flex shrink-0 text-ink-faint transition-transform"
          style={{ transform: open ? "rotate(90deg)" : undefined }}
        >
          <IconChevronRight size={14} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-[13.5px] font-semibold leading-snug">{set.question}</span>
          <span className="mt-1 flex flex-wrap items-center gap-1.5">
            <Chip tone={set.phase === "piping" ? "info" : set.phase === "decided" ? "success" : "neutral"}>{set.phase}</Chip>
            {set.taskClass && <Chip tone="neutral">{set.taskClass}</Chip>}
          </span>
          <span className="mt-1 block text-xs leading-[1.45] text-ink-soft">{phaseLine(set)}</span>
        </span>
      </button>

      {open && (
        <div className="space-y-3 border-t px-3 pb-3 pt-2.5" style={{ borderColor: "var(--ligus-line)" }}>
          <div className="text-xs text-ink-soft">
            Round {set.round + 1} · support decays {set.decayPerRound}/round
            {set.scoutingDeadline && set.phase === "scouting" && (
              <> · tie-break {set.deadlinePassed ? "due" : relTime(set.scoutingDeadline)}</>
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
                sessionId={sessionId}
              />
            ))}
          </div>

          {live && (
            <div className="flex flex-wrap gap-2 border-t pt-3" style={{ borderColor: "var(--ligus-line)" }}>
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
              <Button disabled={busy} onClick={() => run(() => closeProposalSet(sessionId, set.id))}>
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
      <div className="mb-1.5 text-xs">
        <span className="font-semibold">Quorum</span>{" "}
        <span className="text-ink-soft">
          {set.agentQuorum} scout{set.agentQuorum === 1 ? "" : "s"} ≥ {set.qualityBar} · {set.requiredApprovals} human
          approval{set.requiredApprovals === 1 ? "" : "s"} ({set.approvalRoleFloor}+)
        </span>
      </div>
      <div
        role="radiogroup"
        aria-label="Fast to careful"
        className="flex gap-0.5 rounded-lg border p-0.5 text-xs"
        style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
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
              className="ligus-press flex-1 rounded-md px-2 py-1 font-medium"
              style={{
                background: on ? "var(--ligus-panel)" : "transparent",
                boxShadow: on ? "var(--ligus-shadow-1)" : undefined,
                color: on ? "var(--ligus-ink)" : "var(--ligus-ink-soft)",
                opacity: disabled && !on ? 0.6 : 1,
              }}
            >
              {d.label}
            </button>
          );
        })}
      </div>
      {set.dial === "custom" && <div className="mt-1 text-[11px] text-ink-soft">Custom knobs</div>}
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
  sessionId,
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
  sessionId: string;
}) {
  const [showWhy, setShowWhy] = useState(false);
  const [diffOpen, setDiffOpen] = useState(false);
  // The alternative's diff lives on its proposal (shares the Review pane's cache).
  const proposals = useQuery({ queryKey: ["proposals", sessionId], queryFn: () => listProposals(sessionId) });
  const diff = (proposals.data ?? []).find((p: ProposalDto) => p.id === alt.proposalId)?.diff;
  const front = set.frontRunner === alt.proposalId;
  const winner = set.winner === alt.proposalId;
  const step = nextStep(set, alt);
  const awaiting = Math.max(0, set.agentQuorum - alt.scouts - alt.pendingScouts.length);
  const pct = Math.round((alt.support / maxSupport) * 100);
  return (
    <div
      className="rounded-lg border p-2.5"
      data-testid={`alternative-${alt.label}`}
      style={{
        borderColor: front || winner ? "var(--tint-cool-line)" : "var(--ligus-line)",
        background: front || winner ? "color-mix(in srgb, var(--tint-cool) 45%, transparent)" : "transparent",
        opacity: alt.active || winner ? 1 : 0.7,
      }}
    >
      <div className="flex items-start gap-2">
        <span
          className="grid h-5 w-5 shrink-0 place-items-center rounded-md font-mono text-[11px] font-semibold"
          style={{ background: "var(--ligus-mist)", color: "var(--ligus-ink-soft)" }}
          aria-label={`Alternative ${alt.label}`}
        >
          {alt.label}
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-[13px] font-semibold leading-snug">{alt.title}</div>
          {(winner || front || alt.stopReason || alt.frozen || alt.reverted) && (
            <div className="mt-1 flex flex-wrap gap-1">
              {winner && <Chip tone="success" icon={<IconCheck size={10} />}>winner</Chip>}
              {front && !winner && <Chip tone="info">front-runner</Chip>}
              {alt.stopReason && <Chip tone="danger">{alt.stopReason === "withdrawn" ? "withdrawn" : "stopped"}</Chip>}
              {alt.frozen && !alt.stopReason && !winner && set.phase !== "decided" && <Chip tone="warn">frozen</Chip>}
              {alt.reverted && <Chip tone="warn">reverted</Chip>}
            </div>
          )}
        </div>
      </div>
      {alt.body && (
        <div data-testid="alt-body" className="mt-1.5 line-clamp-3 text-ink-soft">
          <Markdown content={alt.body} size="xs" />
        </div>
      )}
      {alt.changedFiles.length > 0 && (
        <div className="mt-1 flex items-center gap-2 text-[11.5px] text-ink-soft">
          <span>
            {alt.changedFiles.length} file{alt.changedFiles.length === 1 ? "" : "s"} changed
          </span>
          {diff && (
            <button
              type="button"
              className="font-medium underline-offset-2 hover:underline"
              style={{ color: "var(--ligus-accent-ink)" }}
              onClick={() => setDiffOpen(true)}
            >
              View diff
            </button>
          )}
        </div>
      )}

      {/* Support: decayed agent support, bar relative to the leader. */}
      <div className="mt-2">
        <div
          className="h-1.5 overflow-hidden rounded-full"
          style={{ background: "color-mix(in srgb, var(--ligus-ink) 10%, transparent)" }}
          role="meter"
          aria-label={`Support for ${alt.label}`}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct}
        >
          <div
            className="h-full rounded-full"
            style={{
              width: `${pct}%`,
              background: alt.meetsQuorum ? "var(--ligus-success)" : "var(--ligus-accent-fill)",
            }}
          />
        </div>
        <div className="mt-1 flex flex-wrap gap-x-1.5 text-[11.5px] text-ink-soft">
          <span>
            {alt.qualifyingScouts}/{set.agentQuorum} scouts over the bar
            {alt.meetsQuorum && <span style={{ color: "var(--ligus-success-ink)" }}> · quorum met</span>}
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
        <div className="mt-2 space-y-1 rounded-md px-2 py-1.5" style={{ background: "var(--ligus-mist)" }}>
          {alt.assessments.map((a) => (
            <div key={a.id} className="flex items-center gap-1.5 text-xs" data-testid="assessment">
              <span className="font-medium">{a.scoutName}</span>
              {a.scoutRuntime && <span className="truncate text-ink-soft">{a.scoutRuntime}</span>}
              <span className="ml-auto font-mono tabular-nums">{supportText(a.score, a.currentSupport)}</span>
              {a.isSelf && <span className="text-ink-soft">(own; not counted)</span>}
            </div>
          ))}
          {alt.pendingScouts.map((n) => (
            <div key={n} className="flex items-center gap-1.5 text-xs text-ink-soft" data-testid="pending-scout">
              <span className="font-medium">{n}</span>
              <span className="ml-auto italic">assessing…</span>
            </div>
          ))}
          {set.phase === "scouting" && alt.active && awaiting > 0 && (
            <div className="text-[11px] italic text-ink-soft">
              {awaiting} more scout{awaiting === 1 ? "" : "s"} needed
            </div>
          )}
          {alt.assessments.some((a) => a.rationale) && (
            <button
              className="text-[11.5px] font-medium text-ink-soft underline-offset-2 hover:text-ink hover:underline"
              onClick={() => setShowWhy((v) => !v)}
              aria-expanded={showWhy}
            >
              {showWhy ? "Hide rationale" : "Why?"}
            </button>
          )}
          {showWhy &&
            alt.assessments
              .filter((a) => a.rationale)
              .map((a) => (
                <div key={a.id} className="text-xs leading-[1.5]">
                  <span className="font-medium">{a.scoutName}:</span>{" "}
                  <div className="text-ink-soft">
                    <Markdown content={a.rationale} size="xs" />
                  </div>
                </div>
              ))}
        </div>
      )}

      {alt.preflight && <Preflight checks={alt.preflight} />}

      {/* The human gate: approvals never decay. */}
      <div className="mt-2.5 flex flex-wrap items-center gap-2">
        <span className="text-[11.5px] tabular-nums text-ink-soft">
          {alt.qualifyingApprovals}/{alt.requiredApprovals} approval{alt.requiredApprovals === 1 ? "" : "s"}
        </span>
        <span className="flex-1" />
        {isOpen(set) && alt.active && !front && (
          <button
            className="text-[11.5px] text-ink-soft underline-offset-2 hover:text-ink hover:underline"
            disabled={busy}
            onClick={onWithdraw}
          >
            Withdraw
          </button>
        )}
        {step === "approve" && (
          <>
            <Button size="sm" variant="danger" disabled={busy} onClick={() => onVote(false)}>
              Reject
            </Button>
            <Button size="sm" disabled={busy} onClick={() => onVote(true)}>
              Approve
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
      </div>
      {diffOpen && diff && (
        <ProposalDiffModal
          title={`${alt.label} · ${alt.title}`}
          diff={diff}
          files={alt.changedFiles.length}
          onClose={() => setDiffOpen(false)}
        />
      )}
    </div>
  );
}

function Preflight({ checks }: { checks: NonNullable<SetAlternativeDto["preflight"]> }) {
  const row = (name: string, c: PreflightCheckDto) => {
    const ok = c.status === "passed";
    const skipped = c.status === "skipped";
    return (
      <div key={name} className="flex items-center gap-1.5 text-xs">
        <span
          aria-hidden
          className="inline-flex"
          style={{ color: ok ? "var(--ligus-success-ink)" : skipped ? "var(--ligus-ink-faint)" : "var(--ligus-danger-ink)" }}
        >
          {ok ? <IconCheck size={12} /> : skipped ? <span className="w-3 text-center">–</span> : <IconX size={12} />}
        </span>
        <span className="font-medium">{name}</span>
        <span className="sr-only">{ok ? "passed" : skipped ? "skipped" : "failed"}</span>
        <span className="truncate text-ink-soft">{c.note}</span>
      </div>
    );
  };
  return (
    <div
      className="mt-2 rounded-md border px-2 py-1.5"
      style={{
        borderColor: checks.passed ? "var(--ligus-success-line)" : "var(--ligus-danger-line)",
        background: checks.passed ? "var(--ligus-success-tint)" : "var(--ligus-danger-tint)",
      }}
      data-testid="preflight"
    >
      <div
        className="mb-1 text-[11.5px] font-semibold"
        style={{ color: checks.passed ? "var(--ligus-success-ink)" : "var(--ligus-danger-ink)" }}
      >
        Pre-flight {checks.passed ? "passed" : "failed"}
      </div>
      {row("Tests", checks.tests)}
      {row("Base unchanged", checks.baseUnchanged)}
      {row("Hosts online", checks.hostsOnline)}
    </div>
  );
}
