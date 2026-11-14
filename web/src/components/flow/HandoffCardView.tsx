import { useState } from "react";
import type { HandoffCard } from "@/lib/flow";
import { qualityLine } from "@/lib/flow";

/// A hand-off card under the message that handed work to another agent:
/// what, where and how good, plus the sender's context parcel if it left one.
export function HandoffCardView({ card }: { card: HandoffCard }) {
  const [open, setOpen] = useState(false);
  const parcel = card.parcel;
  const hasParcel = Boolean(
    parcel && ((parcel.keyFiles?.length ?? 0) + (parcel.decisions?.length ?? 0) + (parcel.ruledOut?.length ?? 0) > 0 || parcel.notes),
  );
  const quality = qualityLine(card.quality);
  return (
    <div
      className="my-1 ml-10 max-w-[70%] rounded-xl border px-3 py-2 text-xs"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", color: "var(--hive-ink)" }}
      data-testid="handoff-card"
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="font-semibold">
          Hand-off · {card.fromName} → {card.toName}
        </span>
        <span
          className="rounded-full px-2 py-0.5 text-[10.5px]"
          style={{ background: "var(--hive-overlay)", color: "var(--hive-ink-soft)" }}
        >
          {card.taskClass}
        </span>
      </div>
      {card.task && <div className="mt-1">{card.task}</div>}
      <div className="mt-1" style={{ color: "var(--hive-ink-soft)" }}>
        {[card.files?.length ? `${card.files.length} file${card.files.length === 1 ? "" : "s"}` : "", quality]
          .filter(Boolean)
          .join(" · ")}
      </div>
      {hasParcel && (
        <div className="mt-1">
          <button
            type="button"
            className="underline-offset-2 hover:underline"
            style={{ color: "var(--hive-accent-ink)" }}
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
          >
            {open ? "Hide context parcel" : "Context parcel"}
          </button>
          {open && parcel && (
            <div className="mt-1 space-y-1">
              <ParcelList title="Key files" items={parcel.keyFiles} />
              <ParcelList title="Decisions" items={parcel.decisions} />
              <ParcelList title="Ruled out" items={parcel.ruledOut} />
              {parcel.notes && <div>{parcel.notes}</div>}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function ParcelList({ title, items }: { title: string; items?: string[] }) {
  if (!items?.length) return null;
  return (
    <div>
      <div className="font-semibold" style={{ color: "var(--hive-ink-soft)" }}>
        {title}
      </div>
      <ul className="ml-4 list-disc">
        {items.map((i) => (
          <li key={i}>{i}</li>
        ))}
      </ul>
    </div>
  );
}
