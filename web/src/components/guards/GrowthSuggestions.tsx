import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { growthSuggestions } from "@/lib/ipc";
import { IconSparkle, IconX } from "@/lib/icons";
import { Banner, LinkButton } from "@/components/ui";

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
    <Banner
      tone="neutral"
      testId="growth-suggestion"
      icon={<span style={{ color: "var(--ligus-accent-ink)" }}><IconSparkle size={14} /></span>}
      title={`${next.title}.`}
      detail={<span title={next.reason}>{next.reason}</span>}
      actions={
        <>
          <LinkButton onClick={() => onOpen(next.kind)}>Set up</LinkButton>
          <button
            type="button"
            aria-label="Dismiss suggestion"
            className="grid h-6 w-6 place-items-center rounded-md text-ink-soft hover:bg-[color:var(--ligus-hover)] hover:text-ink"
            onClick={dismiss}
          >
            <IconX size={13} />
          </button>
        </>
      }
    />
  );
}
