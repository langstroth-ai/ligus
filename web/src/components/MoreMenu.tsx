import { useRef, useState, type ReactNode } from "react";
import { IconEllipsis } from "@/lib/icons";
import { Popover } from "@/components/ui";

export type MoreMenuItem = {
  label: string;
  onSelect: () => void;
  /// A destructive action: danger ink, listed last by convention.
  danger?: boolean;
  disabled?: boolean;
  /// Shown on hover; for a disabled item, why it's disabled.
  title?: string;
  glyph?: ReactNode;
};

/// A quiet `⋯` button that opens a small menu of secondary actions, so rare
/// or destructive actions (Delete, Remove) stay off the card until asked for.
export function MoreMenu({
  label,
  items,
  disabled,
}: {
  /// Accessible name of the button, e.g. "More actions for workflow Nightly".
  label: string;
  items: MoreMenuItem[];
  disabled?: boolean;
}) {
  const anchorRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        ref={anchorRef}
        type="button"
        aria-label={label}
        title="More actions"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
        className="grid h-7 w-7 shrink-0 place-items-center rounded-lg text-ink-soft transition-colors hover:bg-[color:var(--ligus-overlay)] hover:text-ink disabled:opacity-50"
      >
        <IconEllipsis size={15} />
      </button>
      {open && (
        <Popover anchorRef={anchorRef} onDismiss={() => setOpen(false)} minWidth={200} align="right">
          {items.map((it, i) => (
            // A plain click (not mousedown) so Enter and Space work too.
            <button
              key={it.label}
              type="button"
              role="menuitem"
              autoFocus={i === 0}
              aria-disabled={it.disabled || undefined}
              title={it.title}
              onClick={() => {
                if (it.disabled) return;
                setOpen(false);
                it.onSelect();
              }}
              className="ligus-press flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-[13px] hover:bg-[color:var(--ligus-overlay)] focus-visible:bg-[color:var(--ligus-overlay)]"
              style={{
                color: it.danger ? "var(--ligus-danger-ink)" : "var(--ligus-ink)",
                opacity: it.disabled ? 0.5 : undefined,
                cursor: it.disabled ? "not-allowed" : undefined,
              }}
            >
              {it.glyph != null && <span className="grid h-5 w-5 shrink-0 place-items-center">{it.glyph}</span>}
              <span className="min-w-0 flex-1 truncate">{it.label}</span>
            </button>
          ))}
        </Popover>
      )}
    </>
  );
}
