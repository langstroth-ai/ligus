import { useEffect } from "react";
import { Modal } from "@/components/ui";
import { errorLogStore, useErrorLog } from "@/components/Toast";

function clock(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/// Every error a toast showed this session, newest first, so one that faded
/// before you read it can still be found. Opening it marks them seen.
export function ErrorLogSheet({ onClose }: { onClose: () => void }) {
  const { entries } = useErrorLog();
  useEffect(() => {
    errorLogStore.markSeen();
  }, [entries.length]);
  const rows = [...entries].reverse();
  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-start justify-center p-4 pt-[10vh]"
      overlayStyle={{ background: "color-mix(in srgb, var(--ligus-ink) 45%, transparent)" }}
      panelClassName="w-full max-w-xl overflow-hidden rounded-2xl border shadow-2xl"
      panelStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
    >
      <div className="flex items-center justify-between border-b px-5 py-3" style={{ borderColor: "var(--ligus-line)" }}>
        <h2 className="ligus-section">Recent errors</h2>
        <div className="flex items-center gap-1">
          {rows.length > 0 && (
            <button
              type="button"
              onClick={() => errorLogStore.clear()}
              className="rounded-md px-2 py-0.5 text-xs text-ink-soft hover:bg-[color:var(--ligus-overlay)]"
            >
              Clear
            </button>
          )}
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-2 py-0.5 text-xs text-ink-soft hover:bg-[color:var(--ligus-overlay)]"
            aria-label="Close recent errors"
          >
            Esc
          </button>
        </div>
      </div>
      {rows.length === 0 ? (
        <p className="px-5 py-6 text-sm text-ink-soft">No errors since Ligus started.</p>
      ) : (
        <ol className="max-h-[60vh] overflow-y-auto px-5 py-3" aria-label="Errors, newest first">
          {rows.map((e) => (
            <li key={e.id} className="flex gap-3 border-b py-2 text-[13px] last:border-b-0" style={{ borderColor: "var(--ligus-line)" }}>
              <time className="shrink-0 tabular-nums text-ink-soft" dateTime={new Date(e.at).toISOString()}>
                {clock(e.at)}
              </time>
              <span className="min-w-0 break-words">{e.message}</span>
            </li>
          ))}
        </ol>
      )}
    </Modal>
  );
}
