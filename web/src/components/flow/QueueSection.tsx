import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { EmptyHint, ErrorState, Section } from "@/components/ui";
import { Muted, Stat } from "@/components/flow/FlowPane";
import { RECRUIT_LABEL, duration, pct, queueReport, type QueueReport } from "@/lib/flow";
import { onWorkspaceSynced } from "@/lib/ipc";

/// Backpressure: the review gate's queue and the measured waits where work is
/// handed on, with a two-sided alert naming where to add capacity and where
/// to slow down.
export function QueueSection() {
  const q = useQuery({ queryKey: ["flow-queue"], queryFn: queueReport });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void q.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [q]);
  if (q.isError) return <ErrorState text="Couldn't load the queue." onRetry={() => void q.refetch()} />;
  if (!q.data) return null;
  return <QueueView report={q.data} />;
}

const EDGE_LABEL = { gate: "Review gate", stage: "Workflow stages", handoff: "Agent hand-offs" } as const;

export function QueueView({ report: r }: { report: QueueReport }) {
  const g = r.gate;
  return (
    <div className="space-y-4">
      {r.alert ? (
        <div
          role="alert"
          className="rounded-xl border px-3 py-2 text-xs"
          style={{ borderColor: "var(--hive-warn)", background: "color-mix(in srgb, var(--hive-accent-warm) 12%, transparent)" }}
        >
          {r.alert}
        </div>
      ) : (
        <Muted>Nothing is backed up.</Muted>
      )}
      <div className="grid grid-cols-2 gap-2">
        <Stat label="Waiting for review" value={`${g.open} / ${r.queueLimit}`} hint="Open proposals against the queue limit" />
        <Stat label="Oldest wait" value={g.open ? duration(g.oldestWaitMs) : "—"} />
        <Stat label="Long waits" value={pct(g.trembleShare)} hint="Share of waiting proposals past the long-wait threshold" />
        <Stat label="Opened / decided (1 h)" value={`${g.openedLastHour} / ${g.decidedLastHour}`} />
      </div>
      <Section title="Waits at hand-off points (7 d)">
        <table className="w-full text-xs" aria-label="Edge waits">
          <thead>
            <tr className="text-left opacity-60">
              <th className="py-1 font-medium">Where</th>
              <th className="font-medium">Median</th>
              <th className="font-medium">Long</th>
              <th className="font-medium">n</th>
            </tr>
          </thead>
          <tbody>
            {r.edges.map((e) => (
              <tr key={e.edge} className="border-t" style={{ borderColor: "var(--hive-line)" }}>
                <td className="py-1">{EDGE_LABEL[e.edge] ?? e.edge}</td>
                <td>{e.samples ? duration(e.medianWaitMs) : "—"}</td>
                <td>{e.samples ? pct(e.trembleShare) : "—"}</td>
                <td>{e.samples}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Section title="Responses">
        <Muted>
          Stop-upstream note: {r.stopUpstream === "ab" ? "on in half the chats (A/B)" : r.stopUpstream}. Asking reviewers and
          offering stages to an idle host are opt-in (Settings → Flow).
        </Muted>
        {r.recruits.length === 0 ? (
          <EmptyHint text="No backpressure responses this week." />
        ) : (
          <ul className="space-y-1 text-xs">
            {r.recruits.map((x) => (
              <li key={x.id}>
                {new Date(x.atMs).toLocaleString()} · {RECRUIT_LABEL[x.action] ?? x.action}
                {x.queueLen > 0 && ` (${x.queueLen} waiting)`}
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}
