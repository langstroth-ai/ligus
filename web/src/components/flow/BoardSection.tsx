import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { Badge, Button, EmptyHint, ErrorState, LoadingState } from "@/components/ui";
import { openChat } from "@/lib/nav";
import { editorStore } from "@/state/editorStore";
import { Muted } from "@/components/flow/FlowPane";
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
      <Muted>
        Unassigned work goes to the agent whose threshold it crosses first, not to the primary. Thresholds differ a little
        per agent, by career and persona. Auto pickup is opt-in (Settings → Flow).
      </Muted>
      {rows.length === 0 ? (
        <EmptyHint text="Nothing waiting: no failing tests, unreviewed proposals, failed stages or review markers." />
      ) : (
        <ul className="space-y-2 text-xs">
          {rows.map((r) => (
            <li key={r.id} className="rounded-lg border px-2.5 py-2" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge tone={r.kind === "failingTests" || r.kind === "failedStage" ? "danger" : "neutral"}>
                  {BOARD_KIND_LABEL[r.kind]}
                </Badge>
                <span className="text-ink-soft">waiting {duration(Math.max(0, now - r.sinceMs))}</span>
              </div>
              <div className="mt-1 break-words text-[12.5px] font-semibold leading-snug">{r.title}</div>
              <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-ink-soft">
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
                <span data-tip="How strongly this item calls for an agent">stimulus {r.stimulus.toFixed(1)}</span>
              </div>
              <div className="mt-2 flex items-center justify-end gap-2">
                {r.claimedBy ? (
                  <Badge tone="success">{r.claimedBy} has it</Badge>
                ) : r.suggested ? (
                  <Button size="sm" onClick={() => onPickup(r)}>
                    Hand to {r.suggestedName}
                  </Button>
                ) : (
                  <span className="text-ink-soft">
                    {r.nearestThreshold != null ? `needs ${r.nearestThreshold.toFixed(1)}` : "no free agent"}
                  </span>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
