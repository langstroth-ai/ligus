import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, ErrorState, Hint, InfoTip, ListRow, RowList, Switch, LoadingState } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { toast, errMsg } from "@/components/Toast";
import { homeReport, publishBeacon, type HomeReport } from "@/lib/flow";

/// The home beacon: one canonical record of where the workspace lives (relay,
/// base branch, coordinating host), so a joining or reconnecting device can
/// check itself against it.
export function HomeSection({ publishing, onPublishing }: { publishing?: boolean; onPublishing?: (v: boolean) => void } = {}) {
  const q = useQuery({ queryKey: ["flow-home"], queryFn: homeReport });
  if (q.isError) return <ErrorState text="Couldn't load the home beacon." onRetry={() => void q.refetch()} />;
  if (!q.data) return <LoadingState rows={2} label="Loading the home beacon" />;
  return (
    <HomeView
      report={q.data}
      publishing={publishing}
      onPublishing={onPublishing}
      onPublish={(note, coordinator) =>
        publishBeacon(note, coordinator)
          .then(() => q.refetch())
          .catch((e) => toast.error(`Couldn't publish: ${errMsg(e)}`))
      }
    />
  );
}

export function HomeView({
  report: r,
  onPublish,
  publishing: publishingProp,
  onPublishing,
}: {
  report: HomeReport;
  onPublish: (note: string, coordinator: boolean) => void;
  /// The "Publish as home" sheet, opened from the pane header.
  publishing?: boolean;
  onPublishing?: (v: boolean) => void;
}) {
  const [note, setNote] = useState(r.beacon?.note ?? "");
  const [coordinator, setCoordinator] = useState(false);
  const [publishingLocal, setPublishingLocal] = useState(false);
  const publishing = publishingProp ?? publishingLocal;
  const setPublishing = onPublishing ?? setPublishingLocal;
  const b = r.beacon;
  return (
    <div className="space-y-4">
      {b ? (
        <RowList label="Home">
          <ListRow title="Relay" meta={b.relayUrl || "none (local only)"} />
          <ListRow title="Chats start from" meta={b.baseBranch || "HEAD"} />
          {b.coordinatorHostId && (
            <ListRow
              title="Coordinator"
              meta={`${r.coordinatorLabel}${r.coordinatorOnline != null ? ` · ${r.coordinatorOnline ? "online" : "offline"}` : ""}`}
            />
          )}
          <ListRow title={`Published by ${b.updatedBy}`} meta={b.note || undefined} />
        </RowList>
      ) : (
        <Hint className="px-0">No home beacon yet.</Hint>
      )}
      {r.mismatches.length > 0 && (
        <div role="alert" className="space-y-1 rounded-xl border px-3 py-2 text-xs" style={{ borderColor: "var(--ligus-warn)" }}>
          {r.mismatches.map((m) => (
            <div key={m}>{m}</div>
          ))}
        </div>
      )}
      {!b && (
        <Button size="sm" onClick={() => setPublishing(true)}>
          Publish as home
        </Button>
      )}
      {publishing && (
      <Sheet
        title="Publish as home"
        description="Publishes this device's setup as the workspace's home. Admins only."
        side="center"
        width={420}
        onClose={() => setPublishing(false)}
      >
      <div className="space-y-2.5">
        <Hint className="px-0">Shares the relay address and base branch, never a token or passphrase.</Hint>
        <input
          aria-label="Home note"
          placeholder="Note (optional)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={{ borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
          value={note}
          onChange={(e) => setNote(e.target.value)}
        />
        <div className="flex items-center justify-between gap-2 text-sm">
          <span className="flex items-center gap-1.5">
            This device coordinates
            <InfoTip label="coordinating" text="The coordinator runs workflows and serves the phone." />
          </span>
          <Switch on={coordinator} onChange={setCoordinator} label="This device coordinates" />
        </div>
        <Button
          size="sm"
          variant="primary"
          onClick={() => {
            onPublish(note.trim(), coordinator);
            setPublishing(false);
          }}
        >
          Publish home
        </Button>
      </div>
      </Sheet>
      )}
    </div>
  );
}
