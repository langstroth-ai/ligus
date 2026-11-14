import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { growthSuggestions } from "@/lib/ipc";
import { IconSparkle, IconX } from "@/lib/icons";

const KEY = "ligus.growth.dismissed";

function loadDismissed(): string[] {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? "[]");
  } catch {
    return [];
  }
}

function saveDismissed(ids: string[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(ids));
  } catch {
    /* private mode: dismissal lasts for this session only */
  }
}

/// A small, dismissible card that suggests adding an agent, a host or a worker
/// when occupancy says so (queued work, a hot plan window, one busy agent),
/// rather than up front. Shows one suggestion at a time.
export function GrowthSuggestions({ onOpen }: { onOpen: (kind: string) => void }) {
  const [dismissed, setDismissed] = useState<string[]>(loadDismissed);
  const q = useQuery({ queryKey: ["growth-suggestions"], queryFn: growthSuggestions, refetchInterval: 5 * 60_000 });
  const next = (q.data ?? []).find((s) => !dismissed.includes(s.id));
  if (!next) return null;
  const dismiss = () => {
    const ids = [...dismissed, next.id];
    setDismissed(ids);
    saveDismissed(ids);
  };
  return (
    <div
      data-testid="growth-suggestion"
      className="flex shrink-0 items-start gap-2 border-b px-4 py-2 text-[12px]"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <span className="mt-0.5" style={{ color: "var(--hive-accent-cool)" }}>
        <IconSparkle size={14} />
      </span>
      <div className="min-w-0 flex-1">
        <span className="font-semibold">{next.title}.</span> <span className="opacity-75">{next.reason}</span>
      </div>
      <button type="button" className="shrink-0 font-medium underline-offset-2 hover:underline" onClick={() => onOpen(next.kind)}>
        Set up
      </button>
      <button type="button" aria-label="Dismiss suggestion" className="shrink-0 opacity-60 hover:opacity-100" onClick={dismiss}>
        <IconX size={14} />
      </button>
    </div>
  );
}
