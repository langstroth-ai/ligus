import { useQuery } from "@tanstack/react-query";
import { ErrorState, EmptyHint, Section } from "@/components/ui";
import { REASON_LABEL, clearRouteStop, minutesLeft, pct, routingReport, type RoutingReport } from "@/lib/flow";
import { Button } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { Muted, Stat } from "@/components/flow/FlowPane";
import { onWorkspaceSynced } from "@/lib/ipc";
import { useEffect } from "react";

/// Routing report: in shadow mode, what the router *would* have picked for
/// un-addressed messages; scores per agent and task class; and outcomes per
/// arm (shadow vs routed) so the switch to live is an informed one.
export function RoutingSection() {
  const report = useQuery({ queryKey: ["flow-routing"], queryFn: () => routingReport(14) });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void report.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [report]);
  if (report.isError) return <ErrorState text="Couldn't load the routing report." onRetry={() => void report.refetch()} />;
  const r = report.data;
  if (!r) return null;
  const clear = (route: string) =>
    clearRouteStop(route)
      .then(() => report.refetch())
      .catch((e) => toast.error(`Couldn't clear the stop: ${errMsg(e)}`));
  return <RoutingReportView report={r} onClearStop={clear} />;
}

export function RoutingReportView({ report: r, onClearStop }: { report: RoutingReport; onClearStop?: (route: string) => void }) {
  const modeText =
    r.mode === "shadow"
      ? "Shadow mode: un-addressed messages still go to the primary; the router only logs its pick."
      : r.mode === "live"
        ? "Live: the router's pick answers un-addressed messages. @mentions always win."
        : "Routing is off. Turn it on in Settings → Flow.";
  return (
    <div className="space-y-4">
      <Muted>{modeText}</Muted>
      <div className="grid grid-cols-2 gap-2">
        <Stat label="Decisions (14 d)" value={r.decisions} />
        <Stat
          label={r.mode === "live" ? "Routed off primary" : "Would route off primary"}
          value={pct(r.offPrimaryShare)}
          hint="Share of decisions whose pick was not the primary"
        />
      </div>

      {r.stops.length > 0 && (
        <Section title="In cool-down">
          <Muted>A severe failure (rate limit, exhausted plan window, provider outage, unreachable server) takes a route out of rotation at once.</Muted>
          <ul className="space-y-1.5 text-xs">
            {r.stops.map((st) => (
              <li key={st.route} className="flex items-center justify-between gap-2 rounded-lg border px-2 py-1.5" style={{ borderColor: "var(--hive-line)" }}>
                <span>
                  <span className="font-medium">{st.name}</span> · {st.reason.replace(/_/g, " ")} · {minutesLeft(st.untilMs)} min left
                </span>
                {onClearStop && (
                  <Button size="sm" onClick={() => onClearStop(st.route)}>
                    Clear
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}

      <Section title="Outcomes by arm">
        {r.arms.length === 0 ? (
          <EmptyHint text="No tagged turns yet." />
        ) : (
          <table className="w-full text-xs" aria-label="Outcomes by arm">
            <thead>
              <tr className="text-left opacity-60">
                <th className="py-1 font-medium">Arm</th>
                <th className="font-medium">Turns</th>
                <th className="font-medium">Accepted</th>
                <th className="font-medium">Rate</th>
                <th className="font-medium">Turns/acc.</th>
                <th className="font-medium">Local</th>
              </tr>
            </thead>
            <tbody>
              {r.arms.map((a) => (
                <tr key={a.arm} className="border-t" style={{ borderColor: "var(--hive-line)" }}>
                  <td className="py-1">{a.arm}</td>
                  <td>{a.turns}</td>
                  <td>{a.accepted}</td>
                  <td>{pct(a.acceptanceRate)}</td>
                  <td>{a.turnsPerAccepted == null ? "—" : a.turnsPerAccepted.toFixed(1)}</td>
                  <td>{pct(a.localShare)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      <Section title="Scores">
        <Muted>From measured outcomes only: approvals, revise loops, stage results, failed turns and test runs. Never an agent's own claims.</Muted>
        {r.scores.length === 0 ? (
          <EmptyHint text="No outcomes yet. Scores appear once proposals are decided." />
        ) : (
          <table className="w-full text-xs" aria-label="Scores">
            <thead>
              <tr className="text-left opacity-60">
                <th className="py-1 font-medium">Class</th>
                <th className="font-medium">Agent</th>
                <th className="font-medium">Score</th>
                <th className="font-medium">Trend</th>
                <th className="font-medium">n</th>
              </tr>
            </thead>
            <tbody>
              {r.scores.map((s) => (
                <tr key={`${s.taskClass}:${s.agent}`} className="border-t" style={{ borderColor: "var(--hive-line)" }}>
                  <td className="py-1">{s.taskClass}</td>
                  <td>{s.agentName}</td>
                  <td>{pct(s.score)}</td>
                  <td title={`recent ${pct(s.recent)}`}>{s.recent < s.score - 0.05 ? "falling" : s.recent > s.score + 0.05 ? "rising" : "steady"}</td>
                  <td>{s.outcomes}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      <Section title="Recent decisions">
        {r.recent.length === 0 ? (
          <EmptyHint text="No un-addressed messages routed yet." />
        ) : (
          <ul className="space-y-1.5 text-xs">
            {r.recent.slice(0, 20).map((d) => (
              <li key={`${d.sessionId}:${d.atMs}`} className="rounded-lg border px-2 py-1.5" style={{ borderColor: "var(--hive-line)" }}>
                <span className="font-medium">{d.taskClass}</span> · {d.mode === "live" ? "sent to" : "would send to"}{" "}
                <span className="font-medium">{d.wouldPick}</span>
                {d.wouldPick !== d.preferred && <> (top score: {d.preferred})</>}
                <span style={{ color: "var(--hive-ink-soft)" }}> · {REASON_LABEL[d.reason]}</span>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}
