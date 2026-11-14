import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  approveJoin,
  getDearthState,
  getHygieneReport,
  getThrottleState,
  runHygieneNow,
  setDearthMode,
  type DearthStateDto,
  type HygieneReportDto,
  type ThrottleStateDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { Button, Section } from "@/components/ui";

const STATE_COLOR: Record<string, string> = {
  open: "var(--hive-ink-soft)",
  allowed: "var(--hive-success)",
  closed: "var(--hive-danger)",
  refused: "var(--hive-danger)",
};

const lines = (s: string) =>
  s
    .split(/[\n,]/)
    .map((x) => x.trim())
    .filter(Boolean);

/// Settings → Guards: dearth mode (the workspace's tight entrance) and the
/// joins waiting on its second approval.
export function GuardsSection() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["dearth-state"], queryFn: getDearthState });
  const d = q.data;
  return (
    <div>
      {d && <DearthPanel d={d} onChanged={() => qc.invalidateQueries({ queryKey: ["dearth-state"] })} />}
      <ThrottlePanel />
      <HygienePanel />
    </div>
  );
}

function DearthPanel({ d, onChanged }: { d: DearthStateDto; onChanged: () => void }) {
  const [reason, setReason] = useState(d.reason);
  const [brood, setBrood] = useState(d.broodPaths.join("\n"));
  const [allowed, setAllowed] = useState(d.allowedMcpServers.join("\n"));
  useEffect(() => {
    setReason(d.reason);
    setBrood(d.broodPaths.join("\n"));
    setAllowed(d.allowedMcpServers.join("\n"));
  }, [d.reason, d.broodPaths.join("\n"), d.allowedMcpServers.join("\n")]);

  const save = useMutation({
    mutationFn: (enabled: boolean) => setDearthMode(enabled, reason, lines(brood), lines(allowed)),
    onSuccess: (_, enabled) => {
      toast.success(enabled ? "Dearth mode is on" : "Dearth mode is off");
      onChanged();
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const approve = useMutation({
    mutationFn: (subject: string) => approveJoin(subject),
    onSuccess: () => {
      toast.success("Approved");
      onChanged();
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  const field = "w-full rounded-lg border px-2.5 py-1.5 text-[13px]";
  const fieldStyle = { borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" };
  return (
    <>
      <Section
        title="Dearth mode"
        action={
          d.canManage ? (
            <Button
              variant={d.enabled ? "ghost" : "primary"}
              disabled={save.isPending}
              onClick={() => save.mutate(!d.enabled)}
            >
              {d.enabled ? "Switch off" : "Switch on"}
            </Button>
          ) : undefined
        }
      >
        <p className="mb-3 text-[13px] opacity-75">
          A tight entrance for hostile periods: a leaked invite, an untrusted repository, a new worker host. While it is
          on, new members and devices need approvals from two different Admins, agents can reach only the MCP servers
          listed below (no web or shell tools; runtimes Ligus can&apos;t confine are refused), and changes to protected
          paths need a human member&apos;s approval before they apply. Everyone sees a banner.
        </p>
        {d.enabled && (
          <p className="mb-3 text-[12px] opacity-70">
            On since {d.setAt ? new Date(d.setAt).toLocaleString() : "now"}
            {d.setBy ? `, by ${d.setBy}` : ""}.
          </p>
        )}
        {!d.canManage && <p className="mb-3 text-[12px] opacity-70">Only Admins and Owners can change this.</p>}
        <label className="mb-1 block text-[12px] font-medium">Reason (shown on the banner)</label>
        <input
          className={`${field} mb-3`}
          style={fieldStyle}
          value={reason}
          disabled={!d.canManage}
          onChange={(e) => setReason(e.target.value)}
          placeholder="e.g. invite link leaked"
        />
        <label className="mb-1 block text-[12px] font-medium">Allowed MCP servers (one per line)</label>
        <textarea
          className={`${field} mb-3 font-mono`}
          style={fieldStyle}
          rows={3}
          value={allowed}
          disabled={!d.canManage}
          onChange={(e) => setAllowed(e.target.value)}
        />
        <label className="mb-1 block text-[12px] font-medium">Protected (brood) paths, globs, one per line</label>
        <textarea
          className={`${field} mb-2 font-mono`}
          style={fieldStyle}
          rows={5}
          value={brood}
          disabled={!d.canManage}
          onChange={(e) => setBrood(e.target.value)}
        />
        {d.enabled && d.canManage && (
          <Button disabled={save.isPending} onClick={() => save.mutate(true)}>
            Save changes
          </Button>
        )}
      </Section>

      <Section title="Entrances">
        <ul className="text-[13px]" data-testid="entrances">
          {d.entrances.map((e) => (
            <li key={`${e.kind}:${e.name}`} className="flex items-center justify-between py-0.5">
              <span>
                <span className="opacity-55">{e.kind}</span> {e.name}
              </span>
              <span style={{ color: STATE_COLOR[e.state] }}>{e.state}</span>
            </li>
          ))}
        </ul>
      </Section>

      {d.pending.length > 0 && (
        <Section title="Waiting for a second approval">
          <ul className="text-[13px]">
            {d.pending.map((p) => (
              <li key={p.subject} className="flex items-center justify-between gap-2 py-1">
                <span>
                  {p.name} <span className="opacity-55">({p.kind}, {p.kind === "device" ? "owned" : "added"} by {p.addedBy || "unknown"})</span>
                </span>
                {p.canApprove ? (
                  <Button disabled={approve.isPending} onClick={() => approve.mutate(p.subject)}>
                    Approve
                  </Button>
                ) : (
                  <span className="text-[12px] opacity-55">needs another Admin</span>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}
    </>
  );
}

/// One-line summary of a sweep. Exported for tests.
export function hygieneSummary(r: HygieneReportDto): string {
  if (!r.ranAt) return "No sweep has run since Ligus started.";
  if (!r.repoBound) return "No git repository is bound to this workspace on this device; nothing to sweep.";
  const n = r.turnWorktreesRemoved + r.chatCheckoutsRemoved + (r.parkedWorktreesRemoved ?? 0);
  return `${n} leftover worktree${n === 1 ? "" : "s"} removed, ${r.staleProposals.length} stale proposal${r.staleProposals.length === 1 ? "" : "s"} set aside.`;
}

function HygienePanel() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["hygiene-report"], queryFn: getHygieneReport });
  const run = useMutation({
    mutationFn: runHygieneNow,
    onSuccess: (r) => {
      qc.setQueryData(["hygiene-report"], r);
      toast.success(hygieneSummary(r));
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const r = q.data;
  return (
    <Section
      title="Hygiene"
      action={
        <Button disabled={run.isPending} onClick={() => run.mutate()}>
          Sweep now
        </Button>
      }
    >
      <p className="mb-2 text-[13px] opacity-75">
        Every few hours Ligus removes worktrees agents left behind (saving any unfinished edits as a patch), removes the
        checkouts of deleted or archived chats (their branches are kept), and sets aside proposals whose changes no longer
        apply to their chat&apos;s branch.
      </p>
      {r && (
        <p className="text-[12px] opacity-70" data-testid="hygiene-summary">
          {hygieneSummary(r)}
          {r.ranAt ? ` Last sweep ${new Date(r.ranAt).toLocaleString()}.` : ""}
        </p>
      )}
      {r && r.staleProposals.length > 0 && (
        <ul className="mt-2 text-[13px]">
          {r.staleProposals.map((p) => (
            <li key={p.proposalId} className="opacity-80">
              Stale: {p.title}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

/// Tone for a window's fill against the thresholds. Exported for tests.
export function windowTone(pct: number, t: ThrottleStateDto): "ok" | "warn" | "bad" {
  if (pct >= t.workflowPct) return "bad";
  if (pct >= t.backgroundPct) return "warn";
  return "ok";
}

const TONE: Record<string, string> = { ok: "var(--hive-success)", warn: "var(--hive-warning, #b7791f)", bad: "var(--hive-danger)" };

function ThrottlePanel() {
  const q = useQuery({ queryKey: ["throttle-state"], queryFn: getThrottleState, refetchInterval: 15000 });
  const t = q.data;
  if (!t) return null;
  return (
    <Section title="Plan windows">
      <p className="mb-2 text-[13px] opacity-75">
        Near a subscription&apos;s usage limit, agents back off in steps instead of all stopping at once: scheduled runs
        pause from about {t.backgroundPct}%, workflow stages and agent hand-offs from about {t.workflowPct}% (each agent
        at a slightly different point). Your own requests always run.
      </p>
      {t.windows.length === 0 ? (
        <p className="text-[12px] opacity-60" data-testid="no-windows">
          No runtime has reported a plan window yet.
        </p>
      ) : (
        <ul className="text-[13px]" data-testid="windows">
          {t.windows.map((w) => (
            <li key={w.runtime} className="flex items-center justify-between py-0.5">
              <span>
                {w.runtime}
                {w.window ? <span className="opacity-55"> · {w.window.replace("_", " ")}</span> : null}
              </span>
              <span style={{ color: TONE[windowTone(w.effectivePct, t)] }}>
                {w.reported ? `${w.effectivePct}%` : w.status}
                {w.resetsAt ? <span className="opacity-60"> · resets {new Date(w.resetsAt).toLocaleTimeString()}</span> : null}
              </span>
            </li>
          ))}
        </ul>
      )}
      {t.deferred.length > 0 && (
        <ul className="mt-2 text-[13px]" data-testid="deferred">
          {t.deferred.map((d) => (
            <li key={`${d.priority}:${d.what}`} className="opacity-80">
              Waiting: {d.what} ({d.runtime} at {d.utilizationPct}%, pauses at {d.thresholdPct}%)
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
