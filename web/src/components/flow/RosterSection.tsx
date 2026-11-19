import { useQuery } from "@tanstack/react-query";
import { EmptyHint, ErrorState, Section, SelectField, Switch, LoadingState } from "@/components/ui";
import { Muted } from "@/components/flow/FlowPane";
import { toast, errMsg } from "@/components/Toast";
import { CAREER_LABEL, rosterReport, setAgentProfile, type Career, type RosterReport, type RosterRow } from "@/lib/flow";

/// Careers and diversity: a new agent starts on low-risk work, graduates on
/// its approval record, and is pulled back to review when the queue is long.
/// A person can pin any stage. Also flags a roster that runs on one runtime.
export function RosterSection() {
  const q = useQuery({ queryKey: ["flow-roster"], queryFn: rosterReport });
  if (q.isError) return <ErrorState text="Couldn't load the roster." onRetry={() => void q.refetch()} />;
  if (!q.data) return <LoadingState rows={2} label="Loading the roster" />;
  const save = (row: RosterRow, patch: { career?: Career | null; boardOptOut?: boolean }) => {
    if (!row.agentId) return;
    const career = patch.career !== undefined ? patch.career : (row.profile?.career ?? null);
    setAgentProfile(row.agentId, career, row.profile?.thresholdBias ?? 0, patch.boardOptOut ?? row.profile?.boardOptOut ?? false)
      .then(() => q.refetch())
      .catch((e) => toast.error(`Couldn't save: ${errMsg(e)}`));
  };
  return <RosterView report={q.data} onChange={save} />;
}

export function RosterView({
  report,
  onChange,
}: {
  report: RosterReport;
  onChange: (row: RosterRow, patch: { career?: Career | null; boardOptOut?: boolean }) => void;
}) {
  const d = report.diversity;
  return (
    <div className="space-y-4">
      <div
        className="rounded-xl border px-3 py-2 text-xs"
        style={{
          borderColor: d.monoculture ? "var(--ligus-warn)" : "var(--ligus-line)",
          background: "var(--ligus-mist)",
        }}
      >
        <div className="font-semibold">
          {d.agents} agent{d.agents === 1 ? "" : "s"} · {d.runtimes} runtime{d.runtimes === 1 ? "" : "s"} · {d.providers} provider
          {d.providers === 1 ? "" : "s"}
        </div>
        <div>{d.advice}</div>
      </div>
      <Muted>New agents start on probation (reviews, docs, tests) and graduate after 5 judged proposals with at least 60% approved. When the review queue is long, the strongest proposer is pulled back to review until it drains.</Muted>
      <Section title="Agents">
        {report.agents.length === 0 ? (
          <EmptyHint text="No named agents yet." />
        ) : (
          <ul className="space-y-2 text-xs">
            {report.agents.map((a) => (
              <li key={a.agentId ?? a.name} className="space-y-1.5 rounded-lg border px-2 py-2" style={{ borderColor: "var(--ligus-line)" }}>
                <div className="flex items-center justify-between gap-2">
                  <span className="font-semibold">{a.name}</span>
                  <span style={{ color: "var(--ligus-ink-soft)" }}>
                    {CAREER_LABEL[a.career]} · {a.why} · {a.approved}/{a.judged} approved
                  </span>
                </div>
                <SelectField
                  value={a.profile?.career ?? "auto"}
                  onChange={(v) => onChange(a, { career: v === "auto" ? null : (v as Career) })}
                  ariaLabel={`Career for ${a.name}`}
                >
                  <option value="auto">Automatic</option>
                  <option value="probation">Pin: probation</option>
                  <option value="proposer">Pin: proposer</option>
                  <option value="reviewer">Pin: reviewer duty</option>
                </SelectField>
                <div className="flex items-center justify-between gap-2">
                  <span>Picks up task-board work</span>
                  <Switch
                    on={!(a.profile?.boardOptOut ?? false)}
                    onChange={(v) => onChange(a, { boardOptOut: !v })}
                    label={`Task board for ${a.name}`}
                  />
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}
