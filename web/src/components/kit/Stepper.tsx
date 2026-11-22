// Where you are in a multi-step flow: numbered steps, the current one
// marked for screen readers, finished ones with a check (never colour alone).

import { IconCheck } from "@/lib/icons";

export function Stepper({ steps, current }: { steps: string[]; current: number }) {
  return (
    <ol className="flex flex-wrap items-center gap-x-2 gap-y-1" aria-label="Steps">
      {steps.map((name, i) => {
        const done = i < current;
        const now = i === current;
        return (
          <li key={name} className="flex items-center gap-2" aria-current={now ? "step" : undefined}>
            {i > 0 && <span aria-hidden className="h-px w-4" style={{ background: "var(--ligus-line)" }} />}
            <span
              aria-hidden
              className="grid h-5 w-5 shrink-0 place-items-center rounded-full border text-[11px] font-semibold tabular-nums"
              style={
                now
                  ? { background: "var(--ligus-accent-fill)", borderColor: "transparent", color: "var(--ligus-on-accent)" }
                  : { borderColor: done ? "var(--ligus-success-line)" : "var(--ligus-line)", color: done ? "var(--ligus-success-ink)" : "var(--ligus-ink-soft)" }
              }
            >
              {done ? <IconCheck size={12} /> : i + 1}
            </span>
            <span
              className="text-[12px]"
              style={{ fontWeight: now ? 600 : 400, color: now ? "var(--ligus-ink)" : "var(--ligus-ink-soft)" }}
            >
              {name}
              {done && <span className="sr-only"> (done)</span>}
            </span>
          </li>
        );
      })}
    </ol>
  );
}
