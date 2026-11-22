// A windowed list for long, scrolling content (transcripts, activity, logs):
// only the rows near the viewport are in the DOM. Rows may have any height;
// each is measured once it renders and the layout corrects itself.

import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";

/// The first and last row indexes to render for a scroll position, with
/// `overscan` rows either side. Exported for tests.
export function visibleRange(offsets: number[], scrollTop: number, viewport: number, overscan: number): [number, number] {
  const n = offsets.length - 1;
  if (n <= 0) return [0, -1];
  // Binary search for the first row whose bottom is below scrollTop.
  let lo = 0;
  let hi = n - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (offsets[mid + 1] <= scrollTop) lo = mid + 1;
    else hi = mid;
  }
  let end = lo;
  while (end < n - 1 && offsets[end + 1] < scrollTop + viewport) end++;
  return [Math.max(0, lo - overscan), Math.min(n - 1, end + overscan)];
}

export function VirtualList<T>({
  items,
  itemKey,
  renderItem,
  estimateHeight = 48,
  overscan = 6,
  className = "",
  style,
  role,
  ariaLabel,
  stickToBottom = false,
}: {
  items: readonly T[];
  itemKey: (item: T, index: number) => string;
  renderItem: (item: T, index: number) => ReactNode;
  /// A guess for rows not yet measured.
  estimateHeight?: number;
  overscan?: number;
  className?: string;
  style?: CSSProperties;
  /// e.g. "log" for a transcript (with aria-relevant="additions").
  role?: string;
  ariaLabel?: string;
  /// Keep the view pinned to the end while new rows arrive, unless the reader
  /// has scrolled up.
  stickToBottom?: boolean;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const heights = useRef(new Map<string, number>());
  const [version, setVersion] = useState(0);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(600);
  const atBottom = useRef(true);

  const keys = useMemo(() => items.map((it, i) => itemKey(it, i)), [items, itemKey]);
  const offsets = useMemo(() => {
    const out = [0];
    for (const k of keys) out.push(out[out.length - 1] + (heights.current.get(k) ?? estimateHeight));
    return out;
    // `version` changes when a measured height changes.
  }, [keys, estimateHeight, version]);
  const total = offsets[offsets.length - 1];
  const [first, last] = visibleRange(offsets, scrollTop, viewport, overscan);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    setViewport(el.clientHeight || 600);
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setViewport(el.clientHeight || 600));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (stickToBottom && el && atBottom.current) el.scrollTop = el.scrollHeight;
  }, [stickToBottom, total]);

  const onHeight = useCallback((key: string, h: number) => {
    if (h > 0 && heights.current.get(key) !== h) {
      heights.current.set(key, h);
      setVersion((v) => v + 1);
    }
  }, []);

  const rows: ReactNode[] = [];
  for (let i = first; i <= last; i++) {
    const k = keys[i];
    rows.push(
      <Row key={k} rowKey={k} index={i} top={offsets[i]} onHeight={onHeight}>
        {renderItem(items[i], i)}
      </Row>,
    );
  }

  return (
    <div
      ref={scrollRef}
      role={role}
      aria-label={ariaLabel}
      aria-relevant={role === "log" ? "additions" : undefined}
      className={`overflow-y-auto ${className}`}
      style={{ position: "relative", ...style }}
      onScroll={(e) => {
        const el = e.currentTarget;
        setScrollTop(el.scrollTop);
        atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
      }}
    >
      <div style={{ height: total, position: "relative" }}>{rows}</div>
    </div>
  );
}

/// One positioned row that reports its height now and whenever it changes
/// (an image loads, a card expands).
function Row({
  rowKey,
  index,
  top,
  onHeight,
  children,
}: {
  rowKey: string;
  index: number;
  top: number;
  onHeight: (key: string, h: number) => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    onHeight(rowKey, el.getBoundingClientRect().height);
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => onHeight(rowKey, el.getBoundingClientRect().height));
    ro.observe(el);
    return () => ro.disconnect();
  }, [rowKey, onHeight]);
  return (
    <div ref={ref} data-index={index} style={{ position: "absolute", top, left: 0, right: 0 }}>
      {children}
    </div>
  );
}
