// The Archived proposals view (Review ⋯ → Archived proposals, ⌘K): every
// archived or hidden-for-me proposal in the workspace, across its chats,
// searchable. Rows, one primary action (open it in its chat), the rest in the
// row menu (web/README.md, "Panes: rows, menus, one primary action").

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { ArchivedProposalDto } from "@/bindings/ArchivedProposalDto";
import { Sheet } from "@/components/kit/Sheet";
import { EmptyHint, ListRow, RowList, fieldStyle } from "@/components/ui";
import type { RowAction } from "@/components/RowMenu";
import { toast, errMsg } from "@/components/Toast";
import { archiveProposals, hideProposals, listArchivedProposals } from "@/lib/ipc";
import { matchesArchived } from "@/lib/proposals";
import { relTime } from "@/lib/time";

/// The row's one muted line: where it is and why it's here.
export function archivedMeta(item: ArchivedProposalDto): string {
  const p = item.proposal;
  const why = p.hidden
    ? "hidden for you"
    : p.archived === "auto"
      ? `auto-archived${p.settledAt ? ` · settled ${relTime(p.settledAt)}` : ""}`
      : `archived${p.archivedBy ? ` by ${p.archivedBy}` : ""}${p.archivedAt ? ` ${relTime(p.archivedAt)}` : ""}`;
  return `${item.chatTitle || "Untitled chat"} · ${why}`;
}

export function ArchivedProposalsSheet({
  onClose,
  onOpen,
}: {
  onClose: () => void;
  /// Open the proposal's chat on its Review pane.
  onOpen: (chatId: string, proposalId: string) => void;
}) {
  const qc = useQueryClient();
  const [query, setQuery] = useState("");
  const items = useQuery({ queryKey: ["archived-proposals"], queryFn: listArchivedProposals });
  const refresh = (chatId: string) => {
    void qc.invalidateQueries({ queryKey: ["archived-proposals"] });
    void qc.invalidateQueries({ queryKey: ["proposals", chatId] });
    // The sidebar's and the inbox's needs-you counts.
    void qc.invalidateQueries({ queryKey: ["chats", "attention"] });
  };
  const restore = async (item: ArchivedProposalDto) => {
    try {
      // Hidden for me: show it again here (still archived, if it also is).
      if (item.proposal.hidden) await hideProposals([item.proposal.id], false);
      else await archiveProposals(item.chatId, [item.proposal.id], false);
      refresh(item.chatId);
    } catch (e) {
      toast.error(`Couldn't restore: ${errMsg(e)}`);
    }
  };
  const shown = (items.data ?? []).filter((i) => matchesArchived(i, query));
  const actions = (item: ArchivedProposalDto): RowAction[] => [
    { label: "Open in chat", onSelect: () => onOpen(item.chatId, item.proposal.id) },
    ...(item.proposal.hidden
      ? [{ label: "Show again", title: "Back in your inbox on this device", onSelect: () => void restore(item) }]
      : [{ label: "Restore", title: "Back in the inbox for everyone", onSelect: () => void restore(item) }]),
  ];
  return (
    <Sheet title="Archived proposals" side="center" width={560} onClose={onClose}>
      <input
        aria-label="Search archived proposals"
        placeholder="Search"
        className="mb-3 w-full rounded-xl border px-3 py-1.5 text-sm"
        style={fieldStyle}
        value={query}
        autoFocus
        onChange={(e) => setQuery(e.target.value)}
      />
      {items.isError && <p className="text-sm text-ink-soft">Couldn't load archived proposals.</p>}
      {items.isSuccess && shown.length === 0 && <EmptyHint text={query ? "No matches" : "Nothing archived"} />}
      {shown.length > 0 && (
        <RowList label="Archived proposals">
          {shown.map((item) => (
            <ListRow
              key={`${item.chatId}/${item.proposal.id}`}
              testId="archived-row"
              title={item.proposal.title}
              meta={archivedMeta(item)}
              trailing={<span className="text-[11px] text-ink-soft">{item.proposal.status}</span>}
              onClick={() => onOpen(item.chatId, item.proposal.id)}
              actions={actions(item)}
            />
          ))}
        </RowList>
      )}
    </Sheet>
  );
}
