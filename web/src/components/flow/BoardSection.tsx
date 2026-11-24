import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { Badge, Button, EmptyHint, ErrorState, InfoTip, LoadingState, OnceHint } from "@/components/ui";
import { openChat } from "@/lib/nav";
import { editorStore } from "@/state/editorStore";
import { toast, errMsg } from "@/components/Toast";
import { BOARD_KIND_LABEL, duration, listBoard, pickupItem, type BoardRow } from "@/lib/flow";
import { onWorkspaceSynced } from "@/lib/ipc";

/// The threshold task board: open work nobody was asked to do. Each item's
/// stimulus (age, severity, how much of its kind waits) is compared with each
/// agent's threshold; the agent it crosses by the most picks it up.
export function BoardSection() {
  const q = useQuery({ queryKey: ["flow-board"], queryFn: listBoard });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void q.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [q]);
  if (q.isError) return <ErrorState text="Couldn't load the task board." onRetry={() => void q.refetch()} />;
  if (q.isPending) return <LoadingState rows={2} label="Loading the task board" />;
  return (
    <BoardView
      rows={q.data ?? []}
      onPickup={(row) =>
        pickupItem(row.id, row.suggested ?? undefined)
          .then(() => {
            toast.success(`Handed to ${row.suggestedName}`);
            return q.refetch();
          })
          .catch((e) => toast.error(`Couldn't hand it over: ${errMsg(e)}`))
      }
    />
  );
}

export function BoardView({ rows, onPickup, now = Date.now() }: { rows: BoardRow[]; onPickup: (row: BoardRow) => void; now?: number }) {
  return (
    <div className="space-y-3">
      <OnceHint id="flow-board">
        Unassigned work goes to the first agent past its threshold.{" "}
        <InfoTip
          label="the task board"
          text="Unassigned work goes to the agent whose threshold it crosses first, not to the primary. Thresholds differ a little per agent, by career and persona. Auto pickup is opt-in (Settings → Flow)."
        />
      </OnceHint>
      {rows.length === 0 ? (
        <EmptyHint text="Nothing waiting." />
      ) : (
        <ul className="ligus-rowlist text-xs" aria-label="Task board">
          {rows.map((r) => (
            <li key={r.id} className="ligus-listrow px-1">
              {/* Title and its pickup on one line; kind, age and where it is
                  on a muted line under it. No boxed tile. */}
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate text-[13px] font-medium" data-tip={r.title}>
                  {r.title}
                </span>
                {r.claimedBy ? (
                  <Badge tone="success">{r.claimedBy} has it</Badge>
                ) : r.suggested ? (
                  <Button size="sm" variant="ghost" onClick={() => onPickup(r)}>
                    Hand to {r.suggestedName}
                  </Button>
                ) : (
                  <span className="shrink-0 text-ink-soft">
                    {r.nearestThreshold != null ? `needs ${r.nearestThreshold.toFixed(1)}` : "no free agent"}
                  </span>
                )}
              </div>
              <div className="mt-0.5 flex min-w-0 items-center gap-x-2 text-ink-soft">
                <Badge tone={r.kind === "failingTests" || r.kind === "failedStage" ? "danger" : "neutral"}>
                  {BOARD_KIND_LABEL[r.kind]}
                </Badge>
                <span className="shrink-0">waiting {duration(Math.max(0, now - r.sinceMs))}</span>
                {r.chatTitle && (
                  <button
                    type="button"
                    className="min-w-0 truncate underline-offset-2 hover:text-ink hover:underline"
                    data-tip={`Open “${r.chatTitle}”`}
                    onClick={() => openChat(r.sessionId)}
                  >
                    in “{r.chatTitle}”
                  </button>
                )}
                {r.files?.[0] && (
                  <button
                    type="button"
                    className="min-w-0 truncate font-mono text-[11px] underline-offset-2 hover:text-ink hover:underline"
                    data-tip={`Open ${r.files[0]} in the editor`}
                    onClick={() => editorStore.requestOpen(r.files![0])}
                  >
                    {r.files[0]}
                  </button>
                )}
                <span className="shrink-0" data-tip="How strongly this item calls for an agent">
                  stimulus {r.stimulus.toFixed(1)}
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
