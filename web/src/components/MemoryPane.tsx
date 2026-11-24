import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Badge, Button, EmptyHint, ErrorState, LoadingState, Section } from "@/components/ui";
import { RailFrame } from "@/components/RightRail";
import { Sheet } from "@/components/kit/Sheet";
import { PANE_DOCS } from "@/lib/docs";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { cleanDisplayName } from "@/lib/names";
import { useRowMenu } from "@/components/RowMenu";
import { onWorkspaceSynced } from "@/lib/ipc";
import {
  canChange,
  decideMemoryProposal,
  draftProblem,
  egressNote,
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

const fieldStyle = { borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-mist)", color: "var(--ligus-ink)" };

function when(ms: number): string {
  return ms > 0 ? new Date(ms).toLocaleString() : "";
}

/// The workspace memory: shared notes and facts every member and agent can
/// read. People write and edit; agents propose, and a person decides.
export function MemoryPane({ sessionId }: { sessionId?: string }) {
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const q = useQuery({ queryKey: ["memory", query], queryFn: () => memoryView(query, []) });
  useEffect(() => {
    let un: (() => void) | undefined;
    void onWorkspaceSynced(() => void q.refetch()).then((u) => (un = u));
    return () => un?.();
  }, [q]);
  const done = () => void q.refetch();
  const fail = (what: string) => (e: unknown) => toast.error(`Couldn't ${what}: ${errMsg(e)}`);
  return (
    <RailFrame
      title="Memory"
      about="Shared notes and facts. Agents read them and propose changes; people decide."
      docs={PANE_DOCS.memory}
      primary={{ label: "New note", onClick: () => setCreating(true) }}
    >
      {q.isError ? (
        <ErrorState text="Couldn't load the workspace memory." onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <LoadingState rows={3} label="Loading memory" />
      ) : (
        <MemoryPaneView
          view={q.data}
          query={query}
          onQuery={setQuery}
          creating={creating}
          onCreating={setCreating}
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
  creating: creatingProp,
  onCreating,
}: {
  view: MemoryView;
  query: string;
  onQuery: (q: string) => void;
  /// The new-note sheet, when the pane header owns the "New note" action.
  creating?: boolean;
  onCreating?: (v: boolean) => void;
  onSave: (d: Draft) => void;
  onRemove: (e: MemoryEntry) => void;
  onDecide: (p: MemoryProposalState, accepted: boolean) => void;
  onPropose: (op: MemoryOp) => void;
}) {
  const [creatingLocal, setCreatingLocal] = useState(false);
  const creating = creatingProp ?? creatingLocal;
  const setCreating = onCreating ?? setCreatingLocal;
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
      {(view.total > 0 || query) && (
        <input
          aria-label="Search memory"
          placeholder="Search notes"
          data-tip="Every word must match"
          className="w-full rounded-xl border px-3 py-1.5 text-sm"
          style={fieldStyle}
          value={query}
          onChange={(e) => onQuery(e.target.value)}
        />
      )}
      {creating && (
        <Sheet title="New note" side="center" width={460} onClose={() => setCreating(false)}>
          <EntryEditor
            onCancel={() => setCreating(false)}
            onSave={(d) => {
              onSave(d);
              setCreating(false);
            }}
          />
        </Sheet>
      )}
      {(view.editedAfterRemoval ?? []).length > 0 && (
        <Section title="Edited after removal">
          <ul className="space-y-2">
            {(view.editedAfterRemoval ?? []).map((e) => (
              <LateEditCard key={e.id} e={e} />
            ))}
          </ul>
        </Section>
      )}
      <Section title={query ? `Matches (${view.entries.length} of ${view.total})` : `Notes (${view.total})`}>
        {view.entries.length === 0 ? (
          <EmptyHint
            text={query ? "No note matches every word." : "No notes yet."}
            action={
              query ? undefined : (
                <Button size="sm" onClick={() => setCreating(true)}>
                  New note
                </Button>
              )
            }
          />
        ) : (
          <ul className="ligus-rowlist" aria-label="Notes">
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
  const restricted = egressNote(e);
  const rowMenu = useRowMenu(
    `Actions for note ${cleanDisplayName(e.current.title)}`,
    mine
      ? [
          {
            label: "Edit",
            onSelect: () =>
              setEditing({ entryId: e.id, base: e.current.rev, title: e.current.title, body: e.current.body, tags: e.current.tags ?? [], resolves: conflicts.map((c) => c.rev) }),
          },
          {
            label: "Remove…",
            danger: true,
            onSelect: () =>
              confirmThen(
                `Remove “${cleanDisplayName(e.current.title)}”? It disappears from the memory for everyone, but its earlier versions stay in the workspace history on every device.`,
                () => onRemove(e),
              ),
          },
        ]
      : [{ label: "Suggest an edit", onSelect: () => setSuggesting(true) }],
  );
  return (
    <li className="px-1 text-sm" data-testid="memory-entry" {...rowMenu.rowProps}>
      {editing && (
        <Sheet title="Edit note" side="center" width={460} onClose={() => setEditing(null)}>
          <EntryEditor
            initial={editing}
            onCancel={() => setEditing(null)}
            onSave={(d) => {
              onSave(d);
              setEditing(null);
            }}
          />
        </Sheet>
      )}
      {suggesting && (
        <Sheet title="Suggest an edit" side="center" width={460} onClose={() => setSuggesting(false)}>
          <EntryEditor
            initial={{ title: e.current.title, body: e.current.body, tags: e.current.tags ?? [] }}
            saveLabel="Propose"
            onCancel={() => setSuggesting(false)}
            onSave={(d) => {
              onPropose({ op: "edit", entryId: e.id, base: e.current.rev, title: d.title, body: d.body, tags: d.tags });
              setSuggesting(false);
            }}
          />
        </Sheet>
      )}
      {(
        <>
          {/* One line: title, muted tags, then any state badge. The body,
              provenance and actions open with the row. */}
          <div className="ligus-listrow flex items-center justify-between gap-2">
            <button
              className="-mx-1 flex min-w-0 flex-1 items-baseline gap-1.5 rounded-lg px-1 text-left transition-colors hover:bg-[color:var(--ligus-overlay)]"
              onClick={() => setExpanded((x) => !x)}
              aria-expanded={expanded}
            >
              <span className="min-w-0 shrink truncate text-[13px] font-medium">
                <bdi>{e.current.title}</bdi>
              </span>
              {(e.current.tags ?? []).length > 0 && (
                <span className="min-w-0 flex-1 truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                  {(e.current.tags ?? []).join(" · ")}
                </span>
              )}
            </button>
            <div className="flex shrink-0 gap-1">
              {restricted && (
                <Badge tone="info" title={restricted}>
                  Restricted
                </Badge>
              )}
              {conflicts.length > 0 && <Badge tone="warn">{conflicts.length === 1 ? "1 conflict" : `${conflicts.length} conflicts`}</Badge>}
              {rowMenu.button}
              {rowMenu.menu}
            </div>
          </div>
          {expanded && (
            <>
              <p className="whitespace-pre-wrap break-words text-xs">{e.current.body}</p>
              <div className="mt-1 pb-2 text-xs" style={{ color: "var(--ligus-ink-soft)" }}>
                {provenance(e.current)} · {when(e.current.atMs)}
                {src && view.chatTitles[src] && ` · from “${view.chatTitles[src]}”`}
                {e.revisionCount > 1 && ` · ${e.revisionCount} versions`}
              </div>
            </>
          )}
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
        </>
      )}
    </li>
  );
}

/// A removed note that someone edited after its removal: the edit wasn't
/// applied (a removed note never comes back), and its text stays in the
/// workspace history only. Shown so it isn't lost silently.
function LateEditCard({ e }: { e: MemoryEntry }) {
  const removed = e.removed;
  return (
    <li className="rounded-lg border px-2.5 py-2 text-xs" style={{ borderColor: "var(--ligus-line)" }} data-testid="memory-late-edit">
      <div>
        A note {removed ? `removed by ${provenance(removed)} on ${when(removed.atMs)}` : "that was removed"} was edited after its removal:
      </div>
      <ul className="mt-0.5 list-disc pl-4" style={{ color: "var(--ligus-ink-soft)" }}>
        {(e.editedAfterRemoval ?? []).map((r) => (
          <li key={r.rev}>
            {provenance(r)} · {when(r.atMs)}
          </li>
        ))}
      </ul>
      <div className="mt-0.5" style={{ color: "var(--ligus-ink-soft)" }}>
        The edit wasn't applied; its text is in the workspace history only. Write a new note if it's still needed.
      </div>
    </li>
  );
}

function ConflictRow({ c, mine, onKeepCurrent, onTakeTheirs }: { c: MemoryRevision; mine: boolean; onKeepCurrent: () => void; onTakeTheirs: () => void }) {
  return (
    <div className="mt-1.5 rounded-md border px-2 py-1.5 text-xs" style={{ borderColor: "var(--ligus-line)" }} data-testid="memory-conflict">
      <div style={{ color: "var(--ligus-ink-soft)" }}>
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
    <li className="rounded-lg border px-2.5 py-2 text-sm" style={{ borderColor: "var(--ligus-line)" }} data-testid="memory-proposal">
      <div className="flex items-center gap-2">
        <Badge tone="info">{verb}</Badge>
        <span className="min-w-0 truncate font-semibold">
          <bdi>{op.op === "remove" ? target?.current.title ?? "a note" : op.title}</bdi>
        </span>
      </div>
      {op.op === "edit" && (
        <div className="mt-0.5 text-xs" style={{ color: "var(--ligus-ink-soft)" }}>
          Changes “<bdi>{target?.current.title ?? "a note no longer in the list"}</bdi>”
        </div>
      )}
      {op.op !== "remove" && <p className="mt-1 whitespace-pre-wrap break-words text-xs">{op.body}</p>}
      {p.proposal.rationale && <p className="mt-1 text-xs italic">Why: {p.proposal.rationale}</p>}
      <div className="mt-1 text-xs" style={{ color: "var(--ligus-ink-soft)" }}>
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
      {problem && (title || body) && <div className="text-xs" style={{ color: "var(--ligus-ink-soft)" }}>{problem}</div>}
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
