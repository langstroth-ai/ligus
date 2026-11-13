import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Field, Section, SelectField, Switch } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { getFlowSettings, setFlowSettings, type FlowSettings, type RouteMode } from "@/lib/flow";

/// Settings → Flow: routing, hand-offs and backpressure. Device-local; a
/// routing decision a device records carries its mode, so every device
/// honours the same pick for a given message.
export function FlowSection() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["flow-settings"], queryFn: getFlowSettings });
  const save = useMutation({
    mutationFn: (next: FlowSettings) => setFlowSettings(next),
    onSuccess: (s) => qc.setQueryData(["flow-settings"], s),
    onError: (e) => toast.error(`Couldn't save flow settings: ${errMsg(e)}`),
  });
  const s = q.data;
  if (!s) return null;
  const set = (patch: Partial<FlowSettings>) => save.mutate({ ...s, ...patch });
  return (
    <>
      <Section title="Routing">
        <Field
          label="Route un-addressed messages"
          hint="Shadow logs what the router would pick while the primary still answers (see Flow → Routing). Live lets the pick answer. An @mention always wins."
        >
          <SelectField value={s.routeMode} onChange={(v) => set({ routeMode: v as RouteMode })} ariaLabel="Routing mode">
            <option value="off">Off</option>
            <option value="shadow">Shadow (log only)</option>
            <option value="live">Live</option>
          </SelectField>
        </Field>
        <Field label="Outcome half-life (days)" hint="How fast old outcomes stop counting in an agent's score.">
          <NumberInput value={s.halfLifeDays} min={1} max={180} onChange={(v) => set({ halfLifeDays: v })} label="Outcome half-life" />
        </Field>
      </Section>
      <Section title="Hand-offs">
        <Row label="Hand-off cards and context parcels" hint="Record what an agent hands on, and give the receiver the sender's summary.">
          <Switch on={s.handoffCards} onChange={(v) => set({ handoffCards: v })} label="Hand-off cards" />
        </Row>
      </Section>
    </>
  );
}

export function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3">
      <div>
        <div className="text-sm">{label}</div>
        {hint && (
          <div className="text-[11px]" style={{ color: "var(--hive-ink-soft)" }}>
            {hint}
          </div>
        )}
      </div>
      {children}
    </div>
  );
}

export function NumberInput({
  value,
  onChange,
  min,
  max,
  step = 1,
  label,
}: {
  value: number;
  onChange: (v: number) => void;
  min: number;
  max: number;
  step?: number;
  label: string;
}) {
  return (
    <input
      type="number"
      aria-label={label}
      className="w-28 rounded-xl border px-3 py-1.5 text-sm"
      style={{ borderColor: "var(--hive-line)", backgroundColor: "var(--hive-mist)", color: "var(--hive-ink)" }}
      value={value}
      min={min}
      max={max}
      step={step}
      onChange={(e) => {
        const v = Number(e.target.value);
        if (Number.isFinite(v)) onChange(Math.min(max, Math.max(min, v)));
      }}
    />
  );
}
