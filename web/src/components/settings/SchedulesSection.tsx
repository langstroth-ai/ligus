import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listRuntimes,
  listSchedules,
  addSchedule,
  removeSchedule,
  setScheduleEnabled,
  type ScheduleTrigger,
} from "@/lib/ipc";
import { Button, EmptyHint, FormDisclosure, ListRow, MetaTag, RowList, Section, Switch, fieldStyle } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";

function triggerSummary(t: ScheduleTrigger): string {
  if (t.kind === "interval") {
    const s = t.every_secs;
    if (s % 3600 === 0) return `every ${s / 3600} h`;
    if (s % 60 === 0) return `every ${s / 60} min`;
    return `every ${s}s`;
  }
  return `daily at ${String(t.hour).padStart(2, "0")}:${String(t.minute).padStart(2, "0")} UTC`;
}

// Scheduled / triggered agents: each fires a prompt on a recurring schedule
// into a fresh chat. Times are UTC (interval triggers are tz-independent).
export function SchedulesSection() {
  const qc = useQueryClient();
  const schedules = useQuery({ queryKey: ["schedules"], queryFn: listSchedules });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });

  const [label, setLabel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [runtimeId, setRuntimeId] = useState("");
  const [mode, setMode] = useState<"interval" | "daily_at">("interval");
  const [everyMin, setEveryMin] = useState("60");
  const [atTime, setAtTime] = useState("09:00");
  const [adding, setAdding] = useState(false);
  const [moreOptions, setMoreOptions] = useState(false);

  const refresh = () => qc.invalidateQueries({ queryKey: ["schedules"] });
  const remove = useMutation({ mutationFn: removeSchedule, onSuccess: refresh });
  const toggle = useMutation({
    mutationFn: (v: { id: string; enabled: boolean }) => setScheduleEnabled(v.id, v.enabled),
    onSuccess: refresh,
  });
  const add = useMutation({
    mutationFn: () => {
      let trigger: ScheduleTrigger;
      if (mode === "interval") {
        const mins = Math.max(1, Math.round(Number(everyMin) || 0));
        trigger = { kind: "interval", every_secs: mins * 60 };
      } else {
        const [h, m] = atTime.split(":").map((n) => parseInt(n, 10));
        trigger = { kind: "daily_at", hour: h || 0, minute: m || 0 };
      }
      return addSchedule({ label, prompt, runtimeId: runtimeId || undefined, trigger });
    },
    onSuccess: () => {
      setLabel("");
      setPrompt("");
      setAdding(false);
      refresh();
    },
    onError: (e) => toast.error(`Couldn't add schedule: ${errMsg(e)}`),
  });

  const field = "w-full rounded-xl border px-3 py-2 text-sm";
  return (
    <Section
      title="Schedules"
      info="Run an agent on a recurring schedule: each fire opens a new chat, posts the prompt, and the agent answers. Times are UTC."
      tag={<MetaTag>UTC</MetaTag>}
      action={
        <Button size="sm" onClick={() => setAdding(true)}>
          Add schedule
        </Button>
      }
    >
      {(schedules.data ?? []).length === 0 ? (
        <EmptyHint text="No schedules yet" />
      ) : (
        <RowList label="Schedules">
          {(schedules.data ?? []).map((s) => (
            <ListRow
              key={s.id}
              title={s.label || "Scheduled run"}
              meta={triggerSummary(s.trigger)}
              tip={s.prompt}
              trailing={
                <Switch
                  on={s.enabled}
                  onChange={(v) => toggle.mutate({ id: s.id, enabled: v })}
                  label={`Enable schedule "${s.label || "Scheduled run"}"`}
                />
              }
              actions={[
                {
                  label: "Remove",
                  danger: true,
                  onSelect: () => confirmThen(`Remove schedule "${s.label || "Scheduled run"}"?`, () => remove.mutate(s.id)),
                },
              ]}
            />
          ))}
        </RowList>
      )}

      {adding && (
        <Sheet
          title="New schedule"
          description="Times are UTC."
          side="center"
          width={460}
          onClose={() => setAdding(false)}
          footer={
            <>
              <Button onClick={() => setAdding(false)}>Cancel</Button>
              <Button variant="primary" disabled={!prompt.trim() || add.isPending} onClick={() => add.mutate()}>
                Add schedule
              </Button>
            </>
          }
        >
          <div className="space-y-2">
            <input
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="Label (e.g. Daily standup digest)"
              aria-label="Label"
              className={field}
              style={fieldStyle}
            />
            <textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="Prompt to send each run…"
              aria-label="Prompt"
              rows={3}
              className={field}
              style={fieldStyle}
            />
            <div className="flex flex-wrap items-center gap-2">
              <select
                value={mode}
                onChange={(e) => setMode(e.target.value as "interval" | "daily_at")}
                aria-label="Trigger"
                className="rounded-xl border px-3 py-2 text-sm"
                style={fieldStyle}
              >
                <option value="interval">Every</option>
                <option value="daily_at">Daily at</option>
              </select>
              {mode === "interval" ? (
                <>
                  <input
                    type="number"
                    min={1}
                    value={everyMin}
                    onChange={(e) => setEveryMin(e.target.value)}
                    aria-label="Minutes"
                    className="w-20 rounded-xl border px-2 py-1.5 text-sm"
                    style={fieldStyle}
                  />
                  <span className="text-sm text-ink-soft">minutes</span>
                </>
              ) : (
                <input
                  type="time"
                  value={atTime}
                  onChange={(e) => setAtTime(e.target.value)}
                  aria-label="Time (UTC)"
                  className="rounded-xl border px-3 py-2 text-sm"
                  style={fieldStyle}
                />
              )}
            </div>
            <FormDisclosure label="More options" open={moreOptions} onToggle={() => setMoreOptions((v) => !v)}>
              <select
                value={runtimeId}
                onChange={(e) => setRuntimeId(e.target.value)}
                aria-label="Runtime"
                className={field}
                style={fieldStyle}
              >
                <option value="">Default runtime</option>
                {(runtimes.data ?? []).map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.label}
                  </option>
                ))}
              </select>
            </FormDisclosure>
          </div>
        </Sheet>
      )}
    </Section>
  );
}
