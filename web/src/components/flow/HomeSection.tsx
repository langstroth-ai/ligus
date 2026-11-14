import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, ErrorState, Section, Switch, LoadingState } from "@/components/ui";
import { Muted } from "@/components/flow/FlowPane";
import { toast, errMsg } from "@/components/Toast";
import { homeReport, publishBeacon, type HomeReport } from "@/lib/flow";

/// The home beacon: one canonical record of where the workspace lives (relay,
/// base branch, coordinating host), so a joining or reconnecting device can
/// check itself against it.
export function HomeSection() {
  const q = useQuery({ queryKey: ["flow-home"], queryFn: homeReport });
  if (q.isError) return <ErrorState text="Couldn't load the home beacon." onRetry={() => void q.refetch()} />;
  if (!q.data) return <LoadingState rows={2} label="Loading the home beacon" />;
  return (
    <HomeView
      report={q.data}
      onPublish={(note, coordinator) =>
        publishBeacon(note, coordinator)
          .then(() => q.refetch())
          .catch((e) => toast.error(`Couldn't publish: ${errMsg(e)}`))
      }
    />
  );
}

export function HomeView({ report: r, onPublish }: { report: HomeReport; onPublish: (note: string, coordinator: boolean) => void }) {
  const [note, setNote] = useState(r.beacon?.note ?? "");
  const [coordinator, setCoordinator] = useState(false);
  const b = r.beacon;
  return (
    <div className="space-y-4">
      {b ? (
        <div className="space-y-1 rounded-xl border px-3 py-2 text-xs" style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}>
          <div>
            <span className="text-ink-soft">Relay </span>
            {b.relayUrl || "none (local only)"}
          </div>
          <div>
            <span className="text-ink-soft">Chats start from </span>
            {b.baseBranch || "HEAD"}
          </div>
          {b.coordinatorHostId && (
            <div>
              <span className="text-ink-soft">Coordinator </span>
              {r.coordinatorLabel}
              {r.coordinatorOnline != null && ` · ${r.coordinatorOnline ? "online" : "offline"}`}
            </div>
          )}
          {b.note && <div>{b.note}</div>}
          <div className="text-ink-soft">Published by {b.updatedBy}</div>
        </div>
      ) : (
        <Muted>No home beacon yet. An admin can publish this device's setup as the workspace's home.</Muted>
      )}
      {r.mismatches.length > 0 && (
        <div role="alert" className="space-y-1 rounded-xl border px-3 py-2 text-xs" style={{ borderColor: "var(--hive-warn)" }}>
          {r.mismatches.map((m) => (
            <div key={m}>{m}</div>
          ))}
        </div>
      )}
      <Section title="Publish this device's setup as home">
        <Muted>Shares the relay address and base branch, never a token or passphrase. Admins only.</Muted>
        <input
          aria-label="Home note"
          placeholder="Note (optional)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={{ borderColor: "var(--hive-line)", backgroundColor: "var(--hive-mist)", color: "var(--hive-ink)" }}
          value={note}
          onChange={(e) => setNote(e.target.value)}
        />
        <div className="flex items-center justify-between gap-2 text-sm">
          <span>This device coordinates (runs workflows, serves the phone)</span>
          <Switch on={coordinator} onChange={setCoordinator} label="This device coordinates" />
        </div>
        <Button size="sm" onClick={() => onPublish(note.trim(), coordinator)}>
          Publish home
        </Button>
      </Section>
    </div>
  );
}
