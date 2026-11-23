// A compact, sortable table for lists of things with several attributes:
// agents, hosts, devices, members. Headers sort on click (aria-sort says
// which way); rows can open on click or Enter.

import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import "./kit.css";

export interface Column<T> {
  key: string;
  header: string;
  render: (row: T) => ReactNode;
  /// A value to sort by; a column without one isn't sortable.
  sortValue?: (row: T) => string | number | null | undefined;
  align?: "left" | "right";
  /// A CSS width, e.g. "30%" or "96px".
  width?: string;
}

type Sort = { key: string; dir: "asc" | "desc" } | null;

export function DataTable<T>({
  columns,
  rows,
  rowKey,
  onRowClick,
  activeKey,
  initialSort = null,
  empty,
  caption,
}: {
  columns: Column<T>[];
  rows: readonly T[];
  rowKey: (row: T) => string;
  onRowClick?: (row: T) => void;
  /// The key of the row to show as selected.
  activeKey?: string;
  initialSort?: Sort;
  /// Shown in place of the body when there are no rows.
  empty?: ReactNode;
  /// Names the table for screen readers (visually hidden).
  caption?: string;
}) {
  const [sort, setSort] = useState<Sort>(initialSort);

  const sorted = useMemo(() => {
    const col = sort && columns.find((c) => c.key === sort.key);
    if (!col?.sortValue) return rows;
    const get = col.sortValue;
    const mul = sort!.dir === "asc" ? 1 : -1;
    return [...rows].sort((a, b) => {
      const x = get(a);
      const y = get(b);
      if (x == null && y == null) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      if (typeof x === "number" && typeof y === "number") return (x - y) * mul;
      return String(x).localeCompare(String(y), undefined, { numeric: true, sensitivity: "base" }) * mul;
    });
  }, [rows, columns, sort]);

  const toggle = (key: string) =>
    setSort((s) => (s?.key === key ? (s.dir === "asc" ? { key, dir: "desc" } : null) : { key, dir: "asc" }));

  return (
    <table className="w-full border-collapse text-[13px]" style={{ color: "var(--ligus-ink)" }}>
      {caption && <caption className="sr-only">{caption}</caption>}
      <thead>
        <tr className="border-b" style={{ borderColor: "var(--ligus-line)" }}>
          {columns.map((c) => {
            const dir = sort?.key === c.key ? sort.dir : null;
            return (
              <th
                key={c.key}
                scope="col"
                aria-sort={dir === "asc" ? "ascending" : dir === "desc" ? "descending" : c.sortValue ? "none" : undefined}
                className={`h-8 px-2.5 text-[11px] font-semibold uppercase ${c.align === "right" ? "text-right" : "text-left"}`}
                style={{ width: c.width, letterSpacing: "0.06em", color: "var(--ligus-ink-soft)" }}
              >
                {c.sortValue ? (
                  <button
                    type="button"
                    onClick={() => toggle(c.key)}
                    className="inline-flex items-center gap-1 rounded uppercase hover:underline"
                    style={{ letterSpacing: "inherit" }}
                  >
                    {c.header}
                    <span aria-hidden className="inline-block w-2 text-[11px]">
                      {dir === "asc" ? "↑" : dir === "desc" ? "↓" : ""}
                    </span>
                  </button>
                ) : (
                  c.header
                )}
              </th>
            );
          })}
        </tr>
      </thead>
      <tbody>
        {sorted.length === 0 && empty ? (
          <tr>
            <td colSpan={columns.length} className="px-2.5 py-4 text-[13px]" style={{ color: "var(--ligus-ink-soft)" }}>
              {empty}
            </td>
          </tr>
        ) : (
          sorted.map((row) => {
            const k = rowKey(row);
            return (
              <tr
                key={k}
                data-active={activeKey === k || undefined}
                tabIndex={onRowClick ? 0 : undefined}
                onClick={onRowClick ? () => onRowClick(row) : undefined}
                onKeyDown={
                  onRowClick
                    ? (e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          onRowClick(row);
                        }
                      }
                    : undefined
                }
                className={`ligus-row border-b ${onRowClick ? "cursor-pointer" : ""}`}
                style={{ borderColor: "var(--ligus-hairline)" }}
              >
                {columns.map((c) => (
                  <td key={c.key} className={`h-[var(--row-h,30px)] px-2.5 py-1.5 ${c.align === "right" ? "text-right tabular-nums" : ""}`}>
                    {c.render(row)}
                  </td>
                ))}
              </tr>
            );
          })
        )}
      </tbody>
    </table>
  );
}
