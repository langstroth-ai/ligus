import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, EmptyHint, ErrorState, Section, SelectField, LoadingState } from "@/components/ui";
import { Muted } from "@/components/flow/FlowPane";
import { toast, errMsg } from "@/components/Toast";
import { MARK_LABEL, addMark, clearMark, listMarks, minutesLeft, type MarkRow } from "@/lib/flow";
import { onWorkspaceSynced } from "@/lib/ipc";

/// Workspace marks: claims, under-construction locks and needs-review
/// markers that agents and people leave on files. Agents see the active ones
/// in their context and leave their own with a [[mark: …]] directive.
export function MarksSection() {
  const q = useQuery({ queryKey: ["flow-marks"], queryFn: listMarks });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void q.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [q]);
  if (q.isError) return <ErrorState text="Couldn't load marks." onRetry={() => void q.refetch()} />;
  if (q.isPending) return <LoadingState rows={2} label="Loading marks" />;
  const done = () => void q.refetch();
  return (
    <MarksView
      marks={q.data ?? []}
      onAdd={(kind, path, note) =>
        addMark(kind, path, note)
          .then(done)
          .catch((e) => toast.error(`Couldn't leave the mark: ${errMsg(e)}`))
      }
      onClear={(id) =>
        clearMark(id)
          .then(done)
          .catch((e) => toast.error(`Couldn't clear the mark: ${errMsg(e)}`))
      }
    />
  );
}

export function MarksView({
  marks,
  onAdd,
  onClear,
}: {
  marks: MarkRow[];
  onAdd: (kind: "claim" | "lock" | "review", path: string, note: string) => void;
  onClear: (id: string) => void;
}) {
  const [kind, setKind] = useState<"claim" | "lock" | "review">("review");
  const [path, setPath] = useState("");
  const [note, setNote] = useState("");
  return (
    <div className="space-y-4">
      <Muted>Agents read active marks on their own schedule, so they coordinate without mentioning each other. Claims last 4 h and locks 1 h unless cleared.</Muted>
      <Section title="Active">
        {marks.length === 0 ? (
          <EmptyHint text="No marks in this workspace." />
        ) : (
          <ul className="space-y-1.5 text-xs">
            {marks.map((m) => (
              <li key={m.id} className="flex items-start justify-between gap-2 rounded-lg border px-2 py-1.5" style={{ borderColor: "var(--ligus-line)" }}>
                <div className="min-w-0">
                  <div>
                    <span className="font-semibold">{MARK_LABEL[m.kind]}</span> · <code className="break-all">{m.path || m.taskId}</code>
                    {m.lines && ` :${m.lines[0]}-${m.lines[1]}`}
                  </div>
                  <div style={{ color: "var(--ligus-ink-soft)" }}>
                    by {m.by}
                    {m.chatTitle && ` in “${m.chatTitle}”`}
                    {m.expiresMs != null && ` · ${minutesLeft(m.expiresMs)} min left`}
                    {m.note && ` — ${m.note}`}
                  </div>
                </div>
                <Button size="sm" onClick={() => onClear(m.id)}>
                  Clear
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Section>
      <Section title="Leave a mark">
        <SelectField value={kind} onChange={(v) => setKind(v as typeof kind)} ariaLabel="Mark kind">
          <option value="review">Needs review</option>
          <option value="claim">Claim</option>
          <option value="lock">Under construction (lock)</option>
        </SelectField>
        <input
          aria-label="Path"
          placeholder="src/parser.rs"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={{ borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
          value={path}
          onChange={(e) => setPath(e.target.value)}
        />
        <input
          aria-label="Note"
          placeholder="Note (optional)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={{ borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
          value={note}
          onChange={(e) => setNote(e.target.value)}
        />
        <Button
          size="sm"
          disabled={!path.trim()}
          onClick={() => {
            onAdd(kind, path.trim(), note.trim());
            setPath("");
            setNote("");
          }}
        >
          Leave mark
        </Button>
      </Section>
    </div>
  );
}
