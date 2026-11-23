// One thing that needs you (a vote, a question, a gate, a failed run you
// started), as a row in the Inbox. Presentational: the caller decides what
// goes in the Inbox and what the actions do.

import type { ReactNode } from "react";
import { SessionStateChip } from "@/components/ui";
import type { SessionState } from "@/lib/sessionState";
import "./kit.css";

export function InboxItem({
  title,
  state = "needsYou",
  context,
  detail,
  when,
  unread = false,
  onOpen,
  actions,
}: {
  title: string;
  /// The glyph and word on the right ("◆ Needs you", "Failed").
  state?: SessionState;
  /// Where it is: a chat or workspace name.
  context?: ReactNode;
  /// One line of what is being asked.
  detail?: ReactNode;
  /// A short time ("2m", "3:14 PM").
  when?: string;
  /// New since you last opened the Inbox (bold title, no badge).
  unread?: boolean;
  onOpen?: () => void;
  /// Quick actions (Open, Answer). Clicking them doesn't open the item.
  actions?: ReactNode;
}) {
  return (
    <div role="listitem" data-unread={unread || undefined} className="ligus-row group/inbox flex items-start gap-2 rounded-xl px-3 py-2.5">
      <button type="button" onClick={onOpen} className="flex min-w-0 flex-1 flex-col gap-0.5 text-left">
        <span className="flex items-center gap-2">
          <span
            className="min-w-0 flex-1 truncate text-[13px]"
            style={{ fontWeight: unread ? 700 : 590, color: "var(--ligus-ink)" }}
          >
            {title}
            {unread && <span className="sr-only"> (new)</span>}
          </span>
          <SessionStateChip state={state} />
        </span>
        {(context || when) && (
          <span className="flex items-center gap-1.5 text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {context && <span className="min-w-0 truncate">{context}</span>}
            {context && when && <span aria-hidden>·</span>}
            {when && <span className="shrink-0 tabular-nums">{when}</span>}
          </span>
        )}
        {detail && (
          <span className="line-clamp-2 text-[12.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {detail}
          </span>
        )}
      </button>
      {actions && <span className="flex shrink-0 items-center gap-1.5 pt-0.5">{actions}</span>}
    </div>
  );
}
