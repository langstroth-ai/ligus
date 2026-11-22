// The one menu: a list of actions opened from a button (Menu) or a right
// click (useContextMenu in ContextMenu.tsx). Keyboard first: focus moves into
// the menu when it opens and back to where it was when it closes; ↑↓, Home
// and End move, Enter or Space picks, Esc and Tab close, typing a letter jumps
// to the next item starting with it. A danger item can ask for a second press
// ("Press again to leave") instead of a separate dialog.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";

export type MenuItemSpec =
  | { type: "separator" }
  | { type: "header"; label: string }
  | {
      type?: "item";
      label: string;
      onClick: () => void;
      icon?: ReactNode;
      /// A shortcut or short note on the right ("⌘K").
      hint?: string;
      danger?: boolean;
      disabled?: boolean;
      /// Ask for a second press before running; the label becomes this text
      /// ("Press again to leave") until the pointer or focus moves on.
      confirm?: string;
      /// A tooltip; for a disabled item, why it's disabled.
      tip?: string;
    };

type Item = Extract<MenuItemSpec, { label: string; onClick: () => void }>;
const isItem = (it: MenuItemSpec): it is Item => it.type === undefined || it.type === "item";

export type MenuPlacement = { at: { x: number; y: number } } | { anchorRef: RefObject<HTMLElement | null>; align?: "left" | "right" };

/// The menu surface. Render it only while open.
export function MenuPanel({
  items,
  placement,
  onClose,
  label,
  minWidth = 176,
}: {
  items: MenuItemSpec[];
  placement: MenuPlacement;
  onClose: () => void;
  /// Names the menu for screen readers ("Workspace actions").
  label?: string;
  minWidth?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [confirming, setConfirming] = useState<number | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const placeKey = "at" in placement ? `${placement.at.x},${placement.at.y}` : `anchor:${placement.align ?? "left"}`;
  const enabled = items.map((it, i) => (isItem(it) && !it.disabled ? i : -1)).filter((i) => i >= 0);

  // Place it at the point or under the anchor, flipped away from the edges.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const pad = 8;
    let left: number;
    let top: number;
    if ("at" in placement) {
      left = placement.at.x;
      top = placement.at.y;
      if (left + width + pad > window.innerWidth) left = Math.max(pad, left - width);
      if (top + height + pad > window.innerHeight) top = Math.max(pad, top - height);
    } else {
      const r = placement.anchorRef.current?.getBoundingClientRect();
      if (!r) return;
      left = placement.align === "right" ? r.right - width : r.left;
      top = r.bottom + 6;
      if (top + height + pad > window.innerHeight) top = Math.max(pad, r.top - height - 6);
      left = Math.min(Math.max(pad, left), window.innerWidth - width - pad);
    }
    setPos({ left, top });
    // Re-place only when where it opens changes, not on every render.
  }, [placeKey, items.length]);

  // Focus the first item on open; give focus back on close.
  useEffect(() => {
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const first = enabled[0];
    (first !== undefined ? buttons.current[first] : ref.current)?.focus();
    return () => {
      if (prev && document.contains(prev)) prev.focus();
    };
    // Only on mount and unmount.
  }, []);

  const focusAt = (i: number | undefined) => {
    if (i === undefined) return;
    buttons.current[i]?.focus();
  };
  const current = () => buttons.current.findIndex((b) => b === document.activeElement);

  const activate = (i: number) => {
    const it = items[i];
    if (!it || !isItem(it) || it.disabled) return;
    if (it.confirm && confirming !== i) {
      setConfirming(i);
      return;
    }
    onCloseRef.current();
    it.onClick();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const at = enabled.indexOf(current());
    const n = enabled.length;
    if (e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      onCloseRef.current();
    } else if (n === 0) {
      return;
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      focusAt(enabled[(at + 1 + n) % n]);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusAt(enabled[at < 0 ? n - 1 : (at - 1 + n) % n]);
    } else if (e.key === "Home") {
      e.preventDefault();
      focusAt(enabled[0]);
    } else if (e.key === "End") {
      e.preventDefault();
      focusAt(enabled[n - 1]);
    } else if (e.key.length === 1 && /\S/.test(e.key) && !e.metaKey && !e.ctrlKey) {
      const ch = e.key.toLowerCase();
      for (let k = 1; k <= n; k++) {
        const i = enabled[(at + k) % n];
        const it = items[i];
        if (isItem(it) && it.label.toLowerCase().startsWith(ch)) {
          focusAt(i);
          break;
        }
      }
    }
  };

  return (
    <>
      {/* Click-away / right-click-away backdrop. */}
      <div
        className="fixed inset-0 z-[1200]"
        aria-hidden
        onMouseDown={(e) => {
          e.preventDefault();
          onCloseRef.current();
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          onCloseRef.current();
        }}
      />
      <div
        ref={ref}
        role="menu"
        aria-label={label}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        className="fixed z-[1201] overflow-hidden rounded-2xl border p-1.5 text-[13px] outline-none"
        style={{
          minWidth,
          left: pos?.left ?? -9999,
          top: pos?.top ?? -9999,
          visibility: pos ? "visible" : "hidden",
          borderColor: "var(--ligus-line)",
          background: "var(--ligus-panel)",
          boxShadow: "var(--ligus-shadow-pop)",
          // A panel-coloured surface, even when opened from the dark sidebar.
          color: "var(--ligus-base-ink)",
          ["--ligus-ink" as string]: "var(--ligus-base-ink)",
          ["--ligus-ink-soft" as string]: "var(--ligus-base-ink-soft)",
          ["--ligus-overlay" as string]: "var(--ligus-base-overlay)",
        }}
      >
        {items.map((it, i) => {
          if (it.type === "separator") return <div key={i} role="separator" className="mx-1 my-1 h-px" style={{ background: "var(--ligus-line)" }} />;
          if (it.type === "header")
            return (
              <div key={i} role="presentation" className="ligus-cap truncate px-2.5 pb-1 pt-1.5">
                {it.label}
              </div>
            );
          const asking = confirming === i;
          return (
            <button
              key={i}
              ref={(b) => {
                buttons.current[i] = b;
              }}
              type="button"
              role="menuitem"
              tabIndex={-1}
              // aria-disabled, not disabled: the item stays hoverable so its
              // tooltip can say why it's off.
              aria-disabled={it.disabled || undefined}
              data-tip={it.tip}
              data-danger={it.danger || undefined}
              onMouseEnter={(e) => !it.disabled && e.currentTarget.focus()}
              onBlur={() => asking && setConfirming(null)}
              onClick={() => activate(i)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  e.stopPropagation();
                  activate(i);
                }
              }}
              className={`flex h-[var(--row-h,30px)] w-full items-center gap-2 rounded-lg px-2.5 text-left outline-none transition-colors focus:bg-[color:var(--ligus-overlay)] ${it.disabled ? "cursor-not-allowed" : ""}`}
              style={{
                color: it.disabled ? "var(--ligus-ink-faint)" : it.danger ? "var(--ligus-danger-ink)" : "var(--ligus-ink)",
                background: asking ? "var(--ligus-danger-tint)" : undefined,
                fontWeight: asking ? 600 : undefined,
              }}
            >
              {it.icon && (
                <span aria-hidden className="inline-flex shrink-0" style={{ color: it.danger ? undefined : "var(--ligus-ink-soft)" }}>
                  {it.icon}
                </span>
              )}
              <span className="min-w-0 flex-1 truncate" aria-live={it.confirm ? "polite" : undefined}>
                {asking ? it.confirm : it.label}
              </span>
              {it.hint && !asking && <span className="shrink-0 text-[11.5px] text-ink-soft">{it.hint}</span>}
            </button>
          );
        })}
      </div>
    </>
  );
}

/// A menu opened from a button. Controlled: pass `open` and close it in
/// `onClose`. The trigger should carry aria-haspopup="menu" and aria-expanded.
export function Menu({
  open,
  anchorRef,
  onClose,
  items,
  label,
  align = "left",
  minWidth,
}: {
  open: boolean;
  anchorRef: RefObject<HTMLElement | null>;
  onClose: () => void;
  items: MenuItemSpec[];
  label?: string;
  align?: "left" | "right";
  minWidth?: number;
}) {
  if (!open) return null;
  return <MenuPanel items={items} placement={{ anchorRef, align }} onClose={onClose} label={label} minWidth={minWidth} />;
}
