import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { growthSuggestions } from "@/lib/ipc";
import { IconSparkle, IconX } from "@/lib/icons";
import { LinkButton } from "@/components/ui";

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

/// A quiet, dismissible tip at the foot of the canvas that suggests adding an
/// agent, a host or a worker when occupancy says so (queued work, a hot plan
/// window, one busy agent), rather than up front. One suggestion at a time.
/// It sits in the status bar, not a banner: top-of-window banners are kept
/// for states that block work (Lockdown, Dearth).
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
      role="note"
      aria-label="Tip"
      data-testid="growth-suggestion"
      className="ligus-enter flex min-w-0 flex-1 items-center gap-1.5 text-[11.5px] text-ink-soft"
    >
      <span aria-hidden className="inline-flex shrink-0 text-ink-faint">
        <IconSparkle size={11} />
      </span>
      <span className="shrink-0 font-medium text-ink">{next.title}.</span>
      <span className="min-w-0 flex-1 truncate" data-tip={next.reason}>
        {next.reason}
      </span>
      <LinkButton onClick={() => onOpen(next.kind)}>Set up</LinkButton>
      <button
        type="button"
        aria-label="Dismiss suggestion"
        data-tip="Dismiss"
        className="grid h-5 w-5 shrink-0 place-items-center rounded-md text-ink-soft hover:bg-[color:var(--ligus-hover)] hover:text-ink"
        onClick={dismiss}
      >
        <IconX size={12} />
      </button>
    </div>
  );
}
