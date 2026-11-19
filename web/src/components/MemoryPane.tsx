import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Badge, Button, EmptyHint, ErrorState, LoadingState, Section } from "@/components/ui";
import { RailFrame } from "@/components/RightRail";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { onWorkspaceSynced } from "@/lib/ipc";
import {
  canChange,
  decideMemoryProposal,
  draftProblem,
  memoryView,
  parseTags,
  proposedBy,
  proposeMemory,
  provenance,
  removeMemory,
  writeMemory,
  type MemoryEntry,
  type MemoryOp,
  type MemoryProposalState,
  type MemoryRevision,
  type MemoryView,
} from "@/lib/memory";

const fieldStyle = { borderColor: "var(--hive-line)", backgroundColor: "var(--hive-mist)", color: "var(--hive-ink)" };

function when(ms: number): string {
  return ms > 0 ? new Date(ms).toLocaleString() : "";
}

/// The workspace memory: shared notes and facts every member and agent can
/// read. People write and edit; agents propose, and a person decides.
export function MemoryPane({ sessionId }: { sessionId?: string }) {
  const [query, setQuery] = useState("");
  const q = useQuery({ queryKey: ["memory", query], queryFn: () => memoryView(query, []) });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void q.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [q]);
  const done = () => void q.refetch();
  const fail = (what: string) => (e: unknown) => toast.error(`Couldn't ${what}: ${errMsg(e)}`);
  return (
    <RailFrame title="Memory" subtitle="Shared notes and facts. Agents read them and propose changes; people decide.">
      {q.isError ? (
        <ErrorState text="Couldn't load the workspace memory." onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <LoadingState rows={3} label="Loading memory" />
      ) : (
        <MemoryPaneView
          view={q.data}
          query={query}
          onQuery={setQuery}
          onSave={(d) =>
            writeMemory({ ...d, sourceChatId: d.entryId ? undefined : sessionId })
              .then(done)
              .catch(fail("save the note"))
          }
          onRemove={(e) => removeMemory(e.id, e.current.rev).then(done).catch(fail("remove the note"))}
          onDecide={(p, accepted) =>
            decideMemoryProposal(p.proposal.proposalId, accepted)
              .then(done)
              .catch(fail(accepted ? "accept the proposal" : "reject the proposal"))
          }
          onPropose={(op) =>
            proposeMemory(op)
              .then(() => {
                toast.success("Proposed. Its author or an Admin decides it.");
                done();
              })
              .catch(fail("propose the change"))
          }
        />
      )}
    </RailFrame>
  );
}

export interface Draft {
  entryId?: string;
  base?: string;
  title: string;
  body: string;
  tags: string[];
  resolves?: string[];
}

export function MemoryPaneView({
  view,
  query,
  onQuery,
  onSave,
  onRemove,
  onDecide,
  onPropose,
}: {
  view: MemoryView;
  query: string;
  onQuery: (q: string) => void;
  onSave: (d: Draft) => void;
  onRemove: (e: MemoryEntry) => void;
  onDecide: (p: MemoryProposalState, accepted: boolean) => void;
  onPropose: (op: MemoryOp) => void;
}) {
  const [creating, setCreating] = useState(false);
  const open = view.proposals.filter((p) => !p.decision);
  return (
    <div className="space-y-4" data-testid="memory-pane">
      {open.length > 0 && (
        <Section title={`Proposals (${open.length})`}>
          <ul className="space-y-2">
            {open.map((p) => (
              <ProposalCard key={p.proposal.proposalId} p={p} view={view} onDecide={onDecide} />
            ))}
          </ul>
        </Section>
      )}
      <div className="flex gap-1.5">
        <input
          aria-label="Search memory"
          placeholder="Search notes (every word must match)"
          className="min-w-0 flex-1 rounded-xl border px-3 py-1.5 text-sm"
          style={fieldStyle}
          value={query}
          onChange={(e) => onQuery(e.target.value)}
        />
        <Button size="sm" variant="primary" onClick={() => setCreating(true)} disabled={creating}>
          New note
        </Button>
      </div>
      {creating && (
        <EntryEditor
          onCancel={() => setCreating(false)}
          onSave={(d) => {
            onSave(d);
            setCreating(false);
          }}
        />
      )}
      <Section title={query ? `Matches (${view.entries.length} of ${view.total})` : `Notes (${view.total})`}>
        {view.entries.length === 0 ? (
          <EmptyHint text={query ? "No note matches every word." : "No notes yet. Add one, or accept an agent's proposal."} />
        ) : (
          <ul className="space-y-2">
            {view.entries.map((e) => (
              <EntryCard key={e.id} e={e} view={view} onSave={onSave} onRemove={onRemove} onPropose={onPropose} />
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}

function EntryCard({
  e,
  view,
  onSave,
  onRemove,
  onPropose,
}: {
  e: MemoryEntry;
  view: MemoryView;
  onSave: (d: Draft) => void;
  onRemove: (e: MemoryEntry) => void;
  onPropose: (op: MemoryOp) => void;
}) {
  const [editing, setEditing] = useState<null | Draft>(null);
  // Someone else's note: a member suggests an edit, which its author or an
  // Admin decides (the `memory_propose` command).
  const [suggesting, setSuggesting] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const mine = canChange(e, view.me, view.moderator);
  const conflicts = e.conflicts ?? [];
  const src = e.current.source?.chatId;
  return (
    <li className="rounded-lg border px-2.5 py-2 text-sm" style={{ borderColor: "var(--hive-line)" }} data-testid="memory-entry">
      {editing ? (
        <EntryEditor
          initial={editing}
          onCancel={() => setEditing(null)}
          onSave={(d) => {
            onSave(d);
            setEditing(null);
          }}
        />
      ) : suggesting ? (
        <EntryEditor
          initial={{ title: e.current.title, body: e.current.body, tags: e.current.tags ?? [] }}
          saveLabel="Propose"
          onCancel={() => setSuggesting(false)}
          onSave={(d) => {
            onPropose({ op: "edit", entryId: e.id, base: e.current.rev, title: d.title, body: d.body, tags: d.tags });
            setSuggesting(false);
          }}
        />
      ) : (
        <>
          <div className="flex items-start justify-between gap-2">
            <button className="min-w-0 text-left font-semibold" onClick={() => setExpanded((x) => !x)} aria-expanded={expanded}>
              <bdi>{e.current.title}</bdi>
            </button>
            {conflicts.length > 0 && <Badge tone="warn">{conflicts.length === 1 ? "1 conflict" : `${conflicts.length} conflicts`}</Badge>}
          </div>
          {(e.current.tags ?? []).length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
              {(e.current.tags ?? []).map((t) => (
                <Badge key={t}>{t}</Badge>
              ))}
            </div>
          )}
          <p className={`mt-1 whitespace-pre-wrap break-words text-xs ${expanded ? "" : "line-clamp-3"}`}>{e.current.body}</p>
          <div className="mt-1 text-xs" style={{ color: "var(--hive-ink-soft)" }}>
            {provenance(e.current)} · {when(e.current.atMs)}
            {src && view.chatTitles[src] && ` · from “${view.chatTitles[src]}”`}
            {e.revisionCount > 1 && ` · ${e.revisionCount} versions`}
          </div>
          {conflicts.map((c) => (
            <ConflictRow
              key={c.rev}
              c={c}
              mine={mine}
              onKeepCurrent={() =>
                onSave({ entryId: e.id, base: e.current.rev, title: e.current.title, body: e.current.body, tags: e.current.tags ?? [], resolves: [c.rev] })
              }
              onTakeTheirs={() =>
                c.removal
                  ? onRemove(e)
                  : onSave({ entryId: e.id, base: e.current.rev, title: c.title, body: c.body, tags: c.tags ?? [], resolves: [c.rev] })
              }
            />
          ))}
          {!mine && (
            <div className="mt-1.5 flex gap-1.5">
              <Button size="sm" onClick={() => setSuggesting(true)}>
                Suggest an edit
              </Button>
            </div>
          )}
          {mine && (
            <div className="mt-1.5 flex gap-1.5">
              <Button
                size="sm"
                onClick={() =>
                  setEditing({ entryId: e.id, base: e.current.rev, title: e.current.title, body: e.current.body, tags: e.current.tags ?? [], resolves: conflicts.map((c) => c.rev) })
                }
              >
                Edit
              </Button>
              <Button
                size="sm"
                onClick={() =>
                  confirmThen(
                    `Remove “${e.current.title}”? It disappears from the memory for everyone, but its earlier versions stay in the workspace history on every device.`,
                    () => onRemove(e),
                  )
                }
              >
                Remove
              </Button>
            </div>
          )}
        </>
      )}
    </li>
  );
}

function ConflictRow({ c, mine, onKeepCurrent, onTakeTheirs }: { c: MemoryRevision; mine: boolean; onKeepCurrent: () => void; onTakeTheirs: () => void }) {
  return (
    <div className="mt-1.5 rounded-md border px-2 py-1.5 text-xs" style={{ borderColor: "var(--hive-line)" }} data-testid="memory-conflict">
      <div style={{ color: "var(--hive-ink-soft)" }}>
        {c.removal ? `${provenance(c)} removed this while it was being edited` : `${provenance(c)} edited an earlier version at the same time:`}
      </div>
      {!c.removal && <p className="mt-0.5 whitespace-pre-wrap break-words">{c.body}</p>}
      {mine && (
        <div className="mt-1 flex gap-1.5">
          <Button size="sm" onClick={onKeepCurrent}>
            Keep current
          </Button>
          <Button size="sm" onClick={onTakeTheirs}>
            {c.removal ? "Remove it" : "Use theirs"}
          </Button>
        </div>
      )}
    </div>
  );
}

function ProposalCard({ p, view, onDecide }: { p: MemoryProposalState; view: MemoryView; onDecide: (p: MemoryProposalState, accepted: boolean) => void }) {
  const op = p.proposal.op;
  const target = op.op === "create" ? undefined : view.entries.find((e) => e.id === op.entryId);
  const src = p.proposal.source?.chatId;
  const verb = op.op === "create" ? "New note" : op.op === "edit" ? "Edit" : "Remove";
  return (
    <li className="rounded-lg border px-2.5 py-2 text-sm" style={{ borderColor: "var(--hive-line)" }} data-testid="memory-proposal">
      <div className="flex items-center gap-2">
        <Badge tone="info">{verb}</Badge>
        <span className="min-w-0 truncate font-semibold">
          <bdi>{op.op === "remove" ? target?.current.title ?? "a note" : op.title}</bdi>
        </span>
      </div>
      {op.op === "edit" && (
        <div className="mt-0.5 text-xs" style={{ color: "var(--hive-ink-soft)" }}>
          Changes “<bdi>{target?.current.title ?? "a note no longer in the list"}</bdi>”
        </div>
      )}
      {op.op !== "remove" && <p className="mt-1 whitespace-pre-wrap break-words text-xs">{op.body}</p>}
      {p.proposal.rationale && <p className="mt-1 text-xs italic">Why: {p.proposal.rationale}</p>}
      <div className="mt-1 text-xs" style={{ color: "var(--hive-ink-soft)" }}>
        by {proposedBy(p, view.agentNames)}
        {src && view.chatTitles[src] && ` in “${view.chatTitles[src]}”`} · {when(p.atMs)}
      </div>
      <div className="mt-1.5 flex gap-1.5">
        <Button size="sm" variant="primary" onClick={() => onDecide(p, true)}>
          Accept
        </Button>
        <Button size="sm" onClick={() => onDecide(p, false)}>
          Reject
        </Button>
      </div>
    </li>
  );
}

function EntryEditor({
  initial,
  onSave,
  onCancel,
  saveLabel = "Save",
}: {
  initial?: Draft;
  onSave: (d: Draft) => void;
  onCancel: () => void;
  saveLabel?: string;
}) {
  const [title, setTitle] = useState(initial?.title ?? "");
  const [body, setBody] = useState(initial?.body ?? "");
  const [tags, setTags] = useState((initial?.tags ?? []).join(", "));
  const problem = draftProblem(title, body);
  return (
    <div className="space-y-1.5" data-testid="memory-editor">
      <input aria-label="Note title" placeholder="Title" className="w-full rounded-xl border px-3 py-1.5 text-sm" style={fieldStyle} value={title} onChange={(e) => setTitle(e.target.value)} />
      <textarea
        aria-label="Note"
        placeholder="What should the team (and its agents) remember?"
        rows={5}
        className="w-full rounded-xl border px-3 py-1.5 text-sm"
        style={fieldStyle}
        value={body}
        onChange={(e) => setBody(e.target.value)}
      />
      <input aria-label="Tags" placeholder="Tags, comma separated" className="w-full rounded-xl border px-3 py-1.5 text-sm" style={fieldStyle} value={tags} onChange={(e) => setTags(e.target.value)} />
      {problem && (title || body) && <div className="text-xs" style={{ color: "var(--hive-ink-soft)" }}>{problem}</div>}
      <div className="flex gap-1.5">
        <Button size="sm" variant="primary" disabled={!!problem} onClick={() => onSave({ ...initial, title: title.trim(), body, tags: parseTags(tags) })}>
          {saveLabel}
        </Button>
        <Button size="sm" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
