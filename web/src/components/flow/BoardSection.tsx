import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, EmptyHint, ErrorState } from "@/components/ui";
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
            <li key={r.id} className="space-y-1 rounded-lg border px-2 py-2" style={{ borderColor: "var(--hive-line)" }}>
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="font-semibold">{r.title}</div>
                  <div style={{ color: "var(--hive-ink-soft)" }}>
                    {BOARD_KIND_LABEL[r.kind]}
                    {r.chatTitle && ` · “${r.chatTitle}”`} · waiting {duration(Math.max(0, now - r.sinceMs))} · stimulus{" "}
                    {r.stimulus.toFixed(1)}
                  </div>
                </div>
                {r.claimedBy ? (
                  <span className="shrink-0" style={{ color: "var(--hive-success-ink)" }}>
                    {r.claimedBy} has it
                  </span>
                ) : r.suggested ? (
                  <Button size="sm" onClick={() => onPickup(r)}>
                    Hand to {r.suggestedName}
                  </Button>
                ) : (
                  <span className="shrink-0" style={{ color: "var(--hive-ink-soft)" }}>
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
