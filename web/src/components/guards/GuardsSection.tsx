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
import { Badge, Button, EmptyHint, ErrorState, Field, LoadingState, Section, type Tone } from "@/components/ui";
import { IconLock } from "@/lib/icons";
import { EgressPresetsPanel } from "./EgressPresetsPanel";

const STATE_TONE: Record<string, Tone> = {
  open: "neutral",
  allowed: "success",
  closed: "danger",
  refused: "danger",
};

const lines = (s: string) =>
  s
    .split(/[\n,]/)
    .map((x) => x.trim())
    .filter(Boolean);

/// Settings → Guards: dearth mode (the workspace's tight entrance), the
/// joins waiting on its second approval, and the opt-in network presets
/// (for the workspace and, when one is open, the current chat).
export function GuardsSection({ chat }: { chat?: { id: string; title: string } | null } = {}) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["dearth-state"], queryFn: getDearthState });
  const d = q.data;
  return (
    <div className="space-y-2">
      {q.isError && <ErrorState text="Couldn't load dearth mode." onRetry={() => void q.refetch()} />}
      {q.isPending && <LoadingState rows={2} label="Loading guards" />}
      {d && <DearthPanel d={d} onChanged={() => qc.invalidateQueries({ queryKey: ["dearth-state"] })} />}
      <EgressPresetsPanel chat={chat} />
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
      <Section title="Dearth mode">
        <div
          className="rounded-xl border p-3"
          style={{
            borderColor: d.enabled ? "var(--hive-warn-line)" : "var(--hive-line)",
            background: d.enabled ? "var(--hive-warn-tint)" : "var(--hive-panel)",
          }}
        >
          <div className="flex flex-wrap items-start gap-3">
            <span
              aria-hidden
              className="grid h-8 w-8 shrink-0 place-items-center rounded-lg"
              style={{ background: d.enabled ? "var(--hive-warn-tint)" : "var(--hive-mist)", color: d.enabled ? "var(--hive-warn-ink)" : "var(--hive-ink-soft)" }}
            >
              <IconLock size={16} />
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-[13.5px] font-semibold">A tight entrance for hostile periods</span>
                <Badge tone={d.enabled ? "warn" : "neutral"}>{d.enabled ? "on" : "off"}</Badge>
              </div>
              <p className="mt-0.5 text-xs leading-[1.5] text-ink-soft">
                For a leaked invite, an untrusted repository or a new worker host.
                {d.enabled && (
                  <>
                    {" "}On since {d.setAt ? new Date(d.setAt).toLocaleString() : "now"}
                    {d.setBy ? `, by ${d.setBy}` : ""}.
                  </>
                )}
              </p>
            </div>
            {d.canManage && (
              <Button
                variant={d.enabled ? "ghost" : "primary"}
                disabled={save.isPending}
                onClick={() => save.mutate(!d.enabled)}
              >
                {d.enabled ? "Switch off" : "Switch on"}
              </Button>
            )}
          </div>
          <ul className="mt-2.5 space-y-1 pl-11 text-xs leading-[1.5]">
            <li>New members and devices need approvals from two different Admins.</li>
            <li>Agents reach only the MCP servers listed below: no web or shell tools; runtimes Ligus can&apos;t confine are refused.</li>
            <li>Changes to protected paths need a human member&apos;s approval before they apply.</li>
            <li>Everyone sees a banner.</li>
          </ul>
        </div>
        {!d.canManage && <p className="text-xs text-ink-soft">Only Admins and Owners can change this.</p>}
        <Field label="Reason (shown on the banner)">
          <input
            className={field}
            style={fieldStyle}
            value={reason}
            disabled={!d.canManage}
            onChange={(e) => setReason(e.target.value)}
            placeholder="e.g. invite link leaked"
          />
        </Field>
        <div className="grid gap-3 md:grid-cols-2">
          <Field label="Allowed MCP servers" hint="One per line.">
            <textarea
              className={`${field} font-mono`}
              style={fieldStyle}
              rows={4}
              value={allowed}
              disabled={!d.canManage}
              onChange={(e) => setAllowed(e.target.value)}
            />
          </Field>
          <Field label="Protected (brood) paths" hint="Globs, one per line.">
            <textarea
              className={`${field} font-mono`}
              style={fieldStyle}
              rows={4}
              value={brood}
              disabled={!d.canManage}
              onChange={(e) => setBrood(e.target.value)}
            />
          </Field>
        </div>
        {d.enabled && d.canManage && (
          <div>
            <Button disabled={save.isPending} onClick={() => save.mutate(true)}>
              Save changes
            </Button>
          </div>
        )}
      </Section>

      {d.pending.length > 0 && (
        <Section title="Waiting for a second approval">
          <ul className="divide-y rounded-xl border" style={{ borderColor: "var(--hive-line)" }}>
            {d.pending.map((p) => (
              <li key={p.subject} className="flex items-center justify-between gap-2 px-3 py-2 text-[13px]" style={{ borderColor: "var(--hive-line)" }}>
                <span className="min-w-0">
                  <span className="font-medium">{p.name}</span>{" "}
                  <span className="text-xs text-ink-soft">
                    {p.kind}, {p.kind === "device" ? "owned" : "added"} by {p.addedBy || "unknown"}
                  </span>
                </span>
                {p.canApprove ? (
                  <Button disabled={approve.isPending} onClick={() => approve.mutate(p.subject)}>
                    Approve
                  </Button>
                ) : (
                  <span className="text-xs text-ink-soft">needs another Admin</span>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}

      <Section title="Entrances">
        <ul className="divide-y rounded-xl border" style={{ borderColor: "var(--hive-line)" }} data-testid="entrances">
          {d.entrances.map((e) => (
            <li key={`${e.kind}:${e.name}`} className="flex items-center justify-between gap-2 px-3 py-2 text-[13px]" style={{ borderColor: "var(--hive-line)" }}>
              <span>
                {e.name} <span className="text-xs text-ink-soft">{e.kind}</span>
              </span>
              <Badge tone={STATE_TONE[e.state] ?? (d.enabled ? "warn" : "neutral")}>{e.state}</Badge>
            </li>
          ))}
        </ul>
      </Section>
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
      <p className="text-xs leading-[1.5] text-ink-soft">
        Every few hours Ligus removes worktrees agents left behind (saving any unfinished edits as a patch), removes the
        checkouts of deleted or archived chats (their branches are kept), and sets aside proposals whose changes no longer
        apply to their chat&apos;s branch.
      </p>
      {r && (
        <p className="text-[13px]" data-testid="hygiene-summary">
          {hygieneSummary(r)}
          {r.ranAt ? ` Last sweep ${new Date(r.ranAt).toLocaleString()}.` : ""}
        </p>
      )}
      {r && r.staleProposals.length > 0 && (
        <ul className="space-y-1 text-[13px]">
          {r.staleProposals.map((p) => (
            <li key={p.proposalId} className="flex items-center gap-2">
              <Badge tone="warn">stale</Badge> {p.title}
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

const TONE: Record<string, Tone> = { ok: "success", warn: "warn", bad: "danger" };

function ThrottlePanel() {
  const q = useQuery({ queryKey: ["throttle-state"], queryFn: getThrottleState, refetchInterval: 15000 });
  const t = q.data;
  if (q.isPending) return <LoadingState rows={1} label="Loading plan windows" />;
  if (!t) return null;
  return (
    <Section title="Plan windows">
      <p className="text-xs leading-[1.5] text-ink-soft">
        Near a subscription&apos;s usage limit, agents back off in steps instead of all stopping at once: scheduled runs
        pause from about {t.backgroundPct}%, workflow stages and agent hand-offs from about {t.workflowPct}% (each agent
        at a slightly different point). Your own requests always run.
      </p>
      {t.windows.length === 0 ? (
        <div data-testid="no-windows">
          <EmptyHint text="No runtime has reported a plan window yet." />
        </div>
      ) : (
        <ul className="divide-y rounded-xl border" style={{ borderColor: "var(--hive-line)" }} data-testid="windows">
          {t.windows.map((w) => {
            const tone = TONE[windowTone(w.effectivePct, t)];
            return (
              <li key={w.runtime} className="px-3 py-2 text-[13px]" style={{ borderColor: "var(--hive-line)" }}>
                <div className="flex items-center justify-between gap-2">
                  <span>
                    <span className="font-medium">{w.runtime}</span>
                    {w.window ? <span className="text-xs text-ink-soft"> · {w.window.replace("_", " ")}</span> : null}
                  </span>
                  <span className="flex items-center gap-2">
                    {w.resetsAt ? <span className="text-xs text-ink-soft">resets {new Date(w.resetsAt).toLocaleTimeString()}</span> : null}
                    <Badge tone={tone}>{w.reported ? `${w.effectivePct}%` : w.status}</Badge>
                  </span>
                </div>
                {w.reported && (
                  <div className="mt-1.5 h-1 overflow-hidden rounded-full" style={{ background: "color-mix(in srgb, var(--hive-ink) 10%, transparent)" }} aria-hidden>
                    <div
                      className="h-full rounded-full"
                      style={{
                        width: `${Math.min(100, Math.max(0, w.effectivePct))}%`,
                        background: tone === "danger" ? "var(--hive-danger)" : tone === "warn" ? "var(--hive-warn)" : "var(--hive-success)",
                      }}
                    />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {t.deferred.length > 0 && (
        <ul className="space-y-1 text-[13px]" data-testid="deferred">
          {t.deferred.map((d) => (
            <li key={`${d.priority}:${d.what}`} className="flex flex-wrap items-center gap-2">
              <Badge tone="warn">waiting</Badge> {d.what} ({d.runtime} at {d.utilizationPct}%, pauses at {d.thresholdPct}%)
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
