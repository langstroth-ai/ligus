// Tooltips: a short label that appears after a pause on hover, or at once on
// keyboard focus, with the shortcut when there is one. Mark any element with
// `data-tip="Close panel"` (and optionally `data-tip-kbd="Esc"`), or spread
// `tip("Close panel", "Esc")`; one <TooltipLayer/> near the app root shows
// them. That replaces the browser's `title` tooltip, which ignores focus,
// can't show a shortcut and doesn't match the theme.
//
// While a tooltip shows, the element is described by it (aria-describedby),
// unless the tooltip only repeats the element's own name. Give icon-only
// controls an aria-label: a tooltip is not a name.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

/// Props that give an element a tooltip.
export function tip(text: string | undefined | null, kbd?: string): { "data-tip"?: string; "data-tip-kbd"?: string } {
  if (!text) return {};
  return kbd ? { "data-tip": text, "data-tip-kbd": kbd } : { "data-tip": text };
}

/// A keyboard shortcut chip: ⌘K, Esc, ⇧↵.
export function Kbd({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <kbd
      className={`inline-flex min-w-[1.5em] items-center justify-center rounded-[5px] border border-b-2 px-1 font-mono text-[11px] leading-[1.5] ${className}`}
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)", color: "var(--ligus-ink-soft)", fontFamily: "var(--ligus-mono)" }}
    >
      {children}
    </kbd>
  );
}

const HOVER_DELAY = 500;
/// Moving from one tooltip to the next within this window shows at once.
const WARM_WINDOW = 300;
const TIP_ID = "ligus-tooltip";

type Shown = { el: HTMLElement; text: string; kbd?: string };

function target(node: EventTarget | null): HTMLElement | null {
  return node instanceof Element ? (node.closest("[data-tip]") as HTMLElement | null) : null;
}

/// Mount once. Shows the tooltip for whatever [data-tip] element is hovered
/// or keyboard-focused.
export function TooltipLayer() {
  const [shown, setShown] = useState<Shown | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number; below: boolean } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastHide = useRef(0);
  const shownRef = useRef<Shown | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const clear = () => {
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
    };
    const show = (el: HTMLElement) => {
      const text = el.dataset.tip;
      if (!text) return;
      const next = { el, text, kbd: el.dataset.tipKbd };
      shownRef.current = next;
      setShown(next);
    };
    const hide = () => {
      clear();
      if (shownRef.current) lastHide.current = Date.now();
      shownRef.current = null;
      setShown(null);
    };
    const onOver = (e: PointerEvent) => {
      const el = target(e.target);
      if (!el || el === shownRef.current?.el) return;
      clear();
      if (shownRef.current || Date.now() - lastHide.current < WARM_WINDOW) {
        show(el);
      } else {
        timer.current = setTimeout(() => show(el), HOVER_DELAY);
      }
    };
    const onOut = (e: PointerEvent) => {
      const el = target(e.target);
      if (!el) return;
      // Still inside the same element (moving between its children).
      if (e.relatedTarget instanceof Node && el.contains(e.relatedTarget)) return;
      hide();
    };
    const onFocusIn = (e: FocusEvent) => {
      const el = target(e.target);
      if (!el) return;
      let keyboard = true;
      try {
        keyboard = (e.target as Element).matches(":focus-visible");
      } catch {
        // Older engines: treat focus as keyboard focus.
      }
      if (!keyboard) return;
      clear();
      show(el);
    };
    const onKey = (e: KeyboardEvent) => {
      // Dismiss without claiming the key, so Esc still closes what's under it.
      if (e.key === "Escape" && shownRef.current) hide();
    };
    document.addEventListener("pointerover", onOver);
    document.addEventListener("pointerout", onOut);
    document.addEventListener("pointerdown", hide, true);
    document.addEventListener("focusin", onFocusIn);
    document.addEventListener("focusout", hide);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);
    return () => {
      clear();
      document.removeEventListener("pointerover", onOver);
      document.removeEventListener("pointerout", onOut);
      document.removeEventListener("pointerdown", hide, true);
      document.removeEventListener("focusin", onFocusIn);
      document.removeEventListener("focusout", hide);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("blur", hide);
    };
  }, []);

  // Describe the element by the tooltip while it shows (not when it only
  // repeats the element's name), and drop the description afterwards.
  useEffect(() => {
    if (!shown) return;
    const { el, text } = shown;
    const name = (el.getAttribute("aria-label") ?? el.textContent ?? "").trim();
    if (name === text.trim()) return;
    const before = el.getAttribute("aria-describedby");
    el.setAttribute("aria-describedby", before ? `${before} ${TIP_ID}` : TIP_ID);
    return () => {
      if (before) el.setAttribute("aria-describedby", before);
      else el.removeAttribute("aria-describedby");
    };
  }, [shown]);

  // Above the element, centred; below when there's no room; inside the window.
  useLayoutEffect(() => {
    if (!shown) {
      setPos(null);
      return;
    }
    const r = shown.el.getBoundingClientRect();
    const box = boxRef.current?.getBoundingClientRect();
    const w = box?.width ?? 120;
    const h = box?.height ?? 26;
    const pad = 6;
    const below = r.top - h - pad < 4;
    const top = below ? r.bottom + pad : r.top - h - pad;
    const left = Math.min(Math.max(4, r.left + r.width / 2 - w / 2), window.innerWidth - w - 4);
    setPos({ left, top, below });
  }, [shown]);

  if (!shown) return null;
  return (
    <div
      ref={boxRef}
      id={TIP_ID}
      role="tooltip"
      className="ligus-enter pointer-events-none fixed z-[1300] flex max-w-[280px] items-center gap-2 rounded-lg border px-2 py-1 text-[12px] leading-[1.35]"
      style={{
        left: pos?.left ?? -9999,
        top: pos?.top ?? -9999,
        visibility: pos ? "visible" : "hidden",
        borderColor: "var(--ligus-line)",
        background: "var(--ligus-panel)",
        color: "var(--ligus-base-ink, var(--ligus-ink))",
        boxShadow: "var(--ligus-shadow-2)",
      }}
    >
      <span>{shown.text}</span>
      {shown.kbd && <Kbd>{shown.kbd}</Kbd>}
    </div>
  );
}
