import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { Badge, type Tone } from "@/components/ui";
import { onWorkspaceSynced } from "@/lib/ipc";
import { TASK_STATUS_LABEL, taskState, type TaskSlotDto } from "@/lib/memory";

/// What the agents on this chat report about their work in progress,
/// updated as they go (`ligus_task_update`). Hidden when nobody reports.
export function TaskStatePanel({ sessionId }: { sessionId: string }) {
  const q = useQuery({ queryKey: ["task-state", sessionId], queryFn: () => taskState(sessionId), refetchInterval: 4000 });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void q.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [q]);
  return <TaskStateView slots={q.data ?? []} />;
}

const TONE: Record<string, Tone> = {
  working: "info",
  blocked: "warn",
  waiting: "warn",
  done: "success",
  abandoned: "neutral",
};

export function TaskStateView({ slots }: { slots: TaskSlotDto[] }) {
  const shown = slots.filter((s) => !s.stale);
  if (shown.length === 0) return null;
  return (
    <div className="mb-2 flex flex-col gap-1.5" data-testid="task-state" aria-label="Task state" role="status" aria-live="polite">
      {shown.map((s) => (
        <div key={`${s.writer}/${s.update.taskId}/${s.update.agentKey}`} className="rounded-xl border px-3 py-1.5 text-xs" style={{ borderColor: "var(--hive-line)", opacity: s.active ? 1 : 0.7 }}>
          <div className="flex items-center gap-2">
            <span className="font-semibold">
              <bdi>{s.agentName}</bdi>
            </span>
            <Badge tone={TONE[s.update.status] ?? "neutral"}>{TASK_STATUS_LABEL[s.update.status]}</Badge>
            {s.update.progress != null && (
              <span className="flex items-center gap-1" aria-label={`${s.update.progress}% done`}>
                <span className="inline-block h-1.5 w-16 overflow-hidden rounded-full" style={{ background: "var(--hive-mist)" }}>
                  <span className="block h-full" style={{ width: `${s.update.progress}%`, background: "var(--hive-accent-fill)" }} />
                </span>
                {s.update.progress}%
              </span>
            )}
            <span className="ml-auto" style={{ color: "var(--hive-ink-soft)" }}>
              {new Date(s.atMs).toLocaleTimeString()}
            </span>
          </div>
          <p className="mt-0.5 break-words" dir="auto">
            {s.update.summary}
          </p>
          {(s.update.openQuestions ?? []).length > 0 && (
            <div className="mt-0.5">
              <span className="font-semibold">Open questions</span>
              <ul className="ml-4 list-disc">
                {(s.update.openQuestions ?? []).map((q, i) => (
                  <li key={i}>{q}</li>
                ))}
              </ul>
            </div>
          )}
          {(s.update.partialResults ?? []).length > 0 && (
            <div className="mt-0.5">
              <span className="font-semibold">So far</span>
              <ul className="ml-4 list-disc">
                {(s.update.partialResults ?? []).map((r, i) => (
                  <li key={i} className="whitespace-pre-wrap">
                    {r}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
