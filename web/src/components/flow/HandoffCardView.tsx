import { useState } from "react";
import type { HandoffCard } from "@/lib/flow";
import { qualityLine } from "@/lib/flow";
import { Badge } from "@/components/ui";
import { IconArrowRight, IconChevronRight } from "@/lib/icons";

/// Jump to the receiving agent's reply to the hand-off and flash it: by id
/// when the card names the reply (it does once the receiver answered), else
/// the receiver's first turn after the hand-off message. Returns false when
/// that turn isn't in the transcript yet.
export function revealHandoffReply(card: HandoffCard): boolean {
  let target: HTMLElement | undefined;
  if (card.replyMessageId) {
    target = document.querySelector<HTMLElement>(`.tt[data-message-id="${CSS.escape(card.replyMessageId)}"]`) ?? undefined;
  } else {
    const turns = Array.from(document.querySelectorAll<HTMLElement>(".tt[data-message-id]"));
    const from = card.messageId ? turns.findIndex((t) => t.dataset.messageId === card.messageId) : -1;
    target = turns.slice(from + 1).find((t) => t.dataset.author === card.toName);
  }
  if (!target) return false;
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  target.scrollIntoView({ block: "center", behavior: reduce ? "auto" : "smooth" });
  target.classList.remove("ligus-flash");
  void target.offsetWidth;
  target.classList.add("ligus-flash");
  target.setAttribute("tabindex", "-1");
  target.focus({ preventScroll: true });
  return true;
}

/// A hand-off card under the message that handed work to another agent:
/// what, where and how good, plus the sender's context parcel if it left one.
/// Sits on the turn's text column (avatar 30 + gap 11 + row padding 12).
export function HandoffCardView({ card }: { card: HandoffCard }) {
  const [open, setOpen] = useState(false);
  const [missing, setMissing] = useState(false);
  const parcel = card.parcel;
  const hasParcel = Boolean(
    parcel && ((parcel.keyFiles?.length ?? 0) + (parcel.decisions?.length ?? 0) + (parcel.ruledOut?.length ?? 0) > 0 || parcel.notes),
  );
  const quality = qualityLine(card.quality);
  const meta = [card.files?.length ? `${card.files.length} file${card.files.length === 1 ? "" : "s"}` : "", quality]
    .filter(Boolean)
    .join(" · ");
  return (
    <div
      className="mb-2 ml-[53px] mr-3 max-w-[560px] rounded-lg border px-3 py-2 text-xs"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
      data-testid="handoff-card"
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="inline-flex items-center gap-1 font-semibold">
          <span className="text-ink-soft">Hand-off</span>
          <span>@{card.fromName}</span>
          <span aria-label="to" className="inline-flex text-ink-faint">
            <IconArrowRight size={12} />
          </span>
          <span>@{card.toName}</span>
        </span>
        {card.taskClass && <Badge>{card.taskClass}</Badge>}
        <span className="flex-1" />
        <button
          type="button"
          className="inline-flex items-center gap-0.5 font-medium underline-offset-2 hover:underline"
          style={{ color: "var(--ligus-accent-ink)" }}
          onClick={() => setMissing(!revealHandoffReply(card))}
          data-tip={`Jump to @${card.toName}'s reply`}
        >
          Go to reply
        </button>
      </div>
      {card.task && <div className="mt-1 text-[12.5px] leading-[1.45]">{card.task}</div>}
      {meta && <div className="mt-0.5 text-ink-soft">{meta}</div>}
      {missing && (
        <div className="mt-0.5 text-ink-soft">
          {card.replyMessageId ? `@${card.toName}'s reply isn't in the loaded part of this chat.` : `@${card.toName} hasn't replied yet.`}
        </div>
      )}
      {hasParcel && (
        <div className="mt-1.5">
          <button
            type="button"
            className="inline-flex items-center gap-1 font-medium text-ink-soft hover:text-ink"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
          >
            <span aria-hidden className="inline-flex transition-transform" style={{ transform: open ? "rotate(90deg)" : undefined }}>
              <IconChevronRight size={12} />
            </span>
            Context parcel
          </button>
          {open && parcel && (
            <div className="mt-1.5 space-y-1.5 border-l pl-3" style={{ borderColor: "var(--ligus-line)" }}>
              <ParcelList title="Key files" items={parcel.keyFiles} mono />
              <ParcelList title="Decisions" items={parcel.decisions} />
              <ParcelList title="Ruled out" items={parcel.ruledOut} />
              {parcel.notes && <div className="leading-[1.45]">{parcel.notes}</div>}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function ParcelList({ title, items, mono = false }: { title: string; items?: string[]; mono?: boolean }) {
  if (!items?.length) return null;
  return (
    <div>
      <div className="text-[11px] font-semibold uppercase tracking-[0.06em] text-ink-soft">{title}</div>
      <ul className="ml-4 list-disc">
        {items.map((i) => (
          <li key={i} className={mono ? "font-mono text-[11px]" : undefined}>
            {i}
          </li>
        ))}
      </ul>
    </div>
  );
}
