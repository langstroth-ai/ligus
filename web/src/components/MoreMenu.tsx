import { useRef, useState, type ReactNode } from "react";
import { IconEllipsis } from "@/lib/icons";
import { Menu } from "@/components/Menu";

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
/// The menu is the shared keyboard-first Menu.
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
        data-tip="More actions"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
        className="grid h-7 w-7 shrink-0 place-items-center rounded-lg text-ink-soft transition-colors hover:bg-[color:var(--ligus-overlay)] hover:text-ink disabled:opacity-50"
      >
        <IconEllipsis size={15} />
      </button>
      <Menu
        open={open}
        anchorRef={anchorRef}
        onClose={() => setOpen(false)}
        align="right"
        minWidth={200}
        label={label}
        items={items.map((it) => ({
          label: it.label,
          onClick: it.onSelect,
          danger: it.danger,
          disabled: it.disabled,
          tip: it.title,
          icon: it.glyph,
        }))}
      />
    </>
  );
}
