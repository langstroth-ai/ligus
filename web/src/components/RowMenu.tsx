// Row actions (web/README.md, "Copy in panels" → Row actions): a list row's
// secondary actions (Edit, Remove, Revoke, Copy, Rename, Set default, …) live
// in one menu instead of floating around the row. Three ways in, one menu:
//
//   - right-click on the row (onContextMenu, at the pointer)
//   - the row's "⋯" button, shown on hover and keyboard focus (always on
//     touch screens), for discoverability, keyboard and touch
//   - Shift+F10 or the Menu key while focus is in the row
//
// The menu is the shared keyboard-first MenuPanel (focus moves in, Esc closes,
// focus returns). Destructive items always go last, after a separator; their
// confirm dialogs and consequence text stay with the caller's onSelect.

import { useRef, useState, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";
import { IconEllipsis } from "@/lib/icons";
import { MenuPanel, type MenuItemSpec } from "@/components/Menu";

export type RowAction = {
  label: string;
  onSelect: () => void;
  /// Destructive: danger ink, listed last after a separator.
  danger?: boolean;
  disabled?: boolean;
  /// Tooltip; for a disabled item, why it's disabled.
  title?: string;
  glyph?: ReactNode;
};

/// Orders actions (safe first, destructive last after a separator) as menu items.
export function rowMenuItems(actions: RowAction[]): MenuItemSpec[] {
  const toItem = (a: RowAction): MenuItemSpec => ({
    label: a.label,
    onClick: a.onSelect,
    danger: a.danger,
    disabled: a.disabled,
    tip: a.title,
    icon: a.glyph,
  });
  const safe = actions.filter((a) => !a.danger).map(toItem);
  const danger = actions.filter((a) => a.danger).map(toItem);
  return safe.length && danger.length ? [...safe, { type: "separator" }, ...danger] : [...safe, ...danger];
}

type Where = { at: { x: number; y: number } } | { anchor: true };

/// Wire a row to its action menu. Spread `rowProps` on the row element, put
/// `button` at the row's trailing edge and render `menu` once.
export function useRowMenu(label: string, actions: RowAction[]) {
  const [where, setWhere] = useState<Where | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const has = actions.length > 0;
  const close = () => setWhere(null);
  const rowProps = has
    ? {
        onContextMenu: (e: MouseEvent) => {
          // Leave the browser menu for text fields (copy/paste).
          if ((e.target as HTMLElement).closest("input, textarea, select")) return;
          e.preventDefault();
          e.stopPropagation();
          setWhere({ at: { x: e.clientX, y: e.clientY } });
        },
        onKeyDown: (e: KeyboardEvent) => {
          if ((e.key === "F10" && e.shiftKey) || e.key === "ContextMenu") {
            if ((e.target as HTMLElement).closest("input, textarea, select")) return;
            e.preventDefault();
            e.stopPropagation();
            setWhere({ anchor: true });
          }
        },
      }
    : {};
  const button = has ? (
    <button
      ref={btnRef}
      type="button"
      aria-label={label}
      aria-haspopup="menu"
      aria-expanded={where !== null}
      data-tip="More actions"
      data-tip-kbd="⇧F10"
      onClick={() => setWhere((w) => (w ? null : { anchor: true }))}
      className="ligus-rowmenu-btn grid h-6 w-6 shrink-0 place-items-center rounded-md text-ink-soft transition-[opacity,color] hover:bg-[color:var(--ligus-overlay)] hover:text-ink"
    >
      <IconEllipsis size={14} />
    </button>
  ) : null;
  const menu =
    where === null ? null : (
      <MenuPanel
        items={rowMenuItems(actions)}
        placement={"at" in where ? { at: where.at } : { anchorRef: btnRef, align: "right" }}
        onClose={close}
        label={label}
        minWidth={180}
      />
    );
  return { rowProps, button, menu, open: where !== null };
}
