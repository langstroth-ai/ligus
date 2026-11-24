import { useQuery } from "@tanstack/react-query";
import { useRowMenu } from "@/components/RowMenu";
import { ErrorState, EmptyHint, InfoTip, Section, LoadingState } from "@/components/ui";
import { REASON_LABEL, clearRouteStop, minutesLeft, pct, routingReport, type RoutingReport } from "@/lib/flow";
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
  if (!r) return <LoadingState rows={2} label="Loading the routing report" />;
  const clear = (route: string) =>
    clearRouteStop(route)
      .then(() => report.refetch())
      .catch((e) => toast.error(`Couldn't clear the stop: ${errMsg(e)}`));
  return <RoutingReportView report={r} onClearStop={clear} />;
}

export function RoutingReportView({ report: r, onClearStop }: { report: RoutingReport; onClearStop?: (route: string) => void }) {
  const modeText =
    r.mode === "shadow"
      ? "Shadow mode: the router only logs its pick."
      : r.mode === "live"
        ? "Live: the router answers un-addressed messages."
        : "Routing is off (Settings → Flow).";
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-1.5">
        <Muted>{modeText}</Muted>
        <InfoTip
          label="routing modes"
          text="Shadow: un-addressed messages still go to the primary and the router only logs its pick. Live: the router's pick answers them. @mentions always win."
        />
      </div>
      <div className="grid grid-cols-2 gap-2">
        <Stat label="Decisions (14 d)" value={r.decisions} />
        <Stat
          label={r.mode === "live" ? "Routed off primary" : "Would route off primary"}
          value={pct(r.offPrimaryShare)}
          hint="Share of decisions whose pick was not the primary"
        />
      </div>

      {r.stops.length > 0 && (
        <Section
          title="In cool-down"
          info="A severe failure (rate limit, exhausted plan window, provider outage, unreachable server) takes a route out of rotation at once."
        >
          <ul className="ligus-rowlist text-xs">
            {r.stops.map((st) => (
              <StopItem key={st.route} st={st} onClearStop={onClearStop} />
            ))}
          </ul>
        </Section>
      )}

      <Section
        title="Exploration"
        info={`The exploration share in force averaged ${pct(r.exploration.meanEpsilon)}; it rises when the best known agent is weak, unproven or slipping.`}
      >
        <Muted>
          {pct(r.exploration.share)} of decisions went to an agent other than the top scorer.
          {r.exploration.exploredJudged > 0 &&
            ` ${r.exploration.exploredWins} of ${r.exploration.exploredJudged} explored turns succeeded` +
              (r.exploration.incumbentRate != null ? `, against ${pct(r.exploration.incumbentRate)} for the top scorer.` : ".")}
        </Muted>
      </Section>

      <Section title="Outcomes by arm">
        {r.arms.length === 0 ? (
          <EmptyHint text="No tagged turns yet." />
        ) : (
          <table className="w-full text-xs" aria-label="Outcomes by arm">
            <thead>
              <tr className="text-left text-ink-soft">
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
                <tr key={a.arm} className="border-t" style={{ borderColor: "var(--ligus-line)" }}>
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

      <Section
        title="Scores"
        info="From measured outcomes only: approvals, revise loops, stage results, failed turns and test runs. Never an agent's own claims."
      >
        {r.scores.length === 0 ? (
          <EmptyHint text="No decided proposals yet." />
        ) : (
          <table className="w-full text-xs" aria-label="Scores">
            <thead>
              <tr className="text-left text-ink-soft">
                <th className="py-1 font-medium">Class</th>
                <th className="font-medium">Agent</th>
                <th className="font-medium">Score</th>
                <th className="font-medium">Trend</th>
                <th className="font-medium">n</th>
              </tr>
            </thead>
            <tbody>
              {r.scores.map((s) => (
                <tr key={`${s.taskClass}:${s.agent}`} className="border-t" style={{ borderColor: "var(--ligus-line)" }}>
                  <td className="py-1">{s.taskClass}</td>
                  <td>{s.agentName}</td>
                  <td>{pct(s.score)}</td>
                  <td data-tip={`recent ${pct(s.recent)}`}>{s.recent < s.score - 0.05 ? "falling" : s.recent > s.score + 0.05 ? "rising" : "steady"}</td>
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
          <ul className="ligus-rowlist text-xs">
            {r.recent.slice(0, 20).map((d) => (
              <li key={`${d.sessionId}:${d.atMs}`} className="ligus-listrow px-1">
                <span className="font-medium">{d.taskClass}</span> · {d.mode === "live" ? "sent to" : "would send to"}{" "}
                <span className="font-medium">{d.wouldPick}</span>
                {d.wouldPick !== d.preferred && <> (top score: {d.preferred})</>}
                <span style={{ color: "var(--ligus-ink-soft)" }}> · {REASON_LABEL[d.reason]}</span>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}

function StopItem({
  st,
  onClearStop,
}: {
  st: { route: string; name: string; reason: string; untilMs: number };
  onClearStop?: (route: string) => void;
}) {
  const menu = useRowMenu(`Actions for stopped route ${st.name}`, onClearStop ? [{ label: "Clear", onSelect: () => onClearStop(st.route) }] : []);
  return (
    <li className="ligus-listrow flex items-center justify-between gap-2 px-1" {...menu.rowProps}>
      <span className="min-w-0 truncate">
        <span className="font-medium">{st.name}</span> · {st.reason.replace(/_/g, " ")} · {minutesLeft(st.untilMs)} min left
      </span>
      {menu.button}
      {menu.menu}
    </li>
  );
}
