import { useState } from "react";
import { MenuPanel, type MenuItemSpec } from "@/components/Menu";

export type { MenuItemSpec } from "@/components/Menu";

type OpenState = { x: number; y: number; items: MenuItemSpec[]; label?: string };

/// Themed right-click menus for any element. Usage:
///
///   const { open, menu } = useContextMenu();
///   <div onContextMenu={(e) => open(e, [{ label: "Rename…", onClick: rename }])}>…</div>
///   {menu}
///
/// The hook owns the open state and the rendered menu (the shared MenuPanel,
/// at the cursor, flipped away from the edges, keyboard-navigable). Render
/// `menu` once anywhere in the component.
export function useContextMenu() {
  const [state, setState] = useState<OpenState | null>(null);
  const open = (e: React.MouseEvent, items: MenuItemSpec[], label?: string) => {
    if (items.length === 0) return;
    e.preventDefault();
    e.stopPropagation();
    setState({ x: e.clientX, y: e.clientY, items, label });
  };
  const close = () => setState(null);
  const menu = state ? <MenuPanel items={state.items} placement={{ at: { x: state.x, y: state.y } }} onClose={close} label={state.label} /> : null;
  return { open, close, menu };
}

/// A menu section header (e.g. the element's name), rendered above items.
export function menuHeader(label: string): MenuItemSpec {
  return { type: "header", label };
}
