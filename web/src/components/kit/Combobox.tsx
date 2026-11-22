// A searchable select: a text field that filters a list of options as you
// type (fuzzy, like the command palette), with the ARIA combobox pattern.
// ↑↓ move, Enter picks, Esc closes. Disabled options stay visible, greyed,
// with their reason, so you can see why something can't be chosen.

import { useId, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { fuzzyRank } from "@/lib/fuzzy";
import { IconCheck, IconChevronDown } from "@/lib/icons";

export interface ComboOption {
  value: string;
  label: string;
  /// A second, quieter line (a host, a runtime, a path).
  hint?: string;
  icon?: ReactNode;
  disabled?: boolean;
  /// Why it can't be chosen; shown under the label.
  disabledReason?: string;
}

export function Combobox({
  label,
  options,
  value,
  onChange,
  placeholder = "Search…",
  emptyText = "No matches",
  hideLabel = false,
}: {
  /// Names the field (shown above it unless hideLabel).
  label: string;
  options: ComboOption[];
  value: string | null;
  onChange: (value: string) => void;
  placeholder?: string;
  emptyText?: string;
  hideLabel?: boolean;
}) {
  const id = useId();
  const listId = `${id}-list`;
  const inputRef = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const selected = options.find((o) => o.value === value) ?? null;

  const shown = useMemo(
    () => (query.trim() ? fuzzyRank(options, query, (o) => `${o.label} ${o.hint ?? ""}`) : options),
    [options, query],
  );

  const close = () => {
    setOpen(false);
    setQuery("");
  };
  const pick = (o: ComboOption | undefined) => {
    if (!o || o.disabled) return;
    onChange(o.value);
    close();
  };
  const move = (delta: number) => {
    if (shown.length === 0) return;
    let i = active;
    for (let n = 0; n < shown.length; n++) {
      i = (i + delta + shown.length) % shown.length;
      if (!shown[i].disabled) break;
    }
    setActive(i);
  };

  return (
    <div className="relative">
      <label htmlFor={id} className={hideLabel ? "sr-only" : "mb-1 block text-[12px] font-medium"}>
        {label}
      </label>
      <div
        className="flex h-8 items-center gap-1.5 rounded-lg border px-2.5"
        style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
      >
        {selected?.icon && !open && <span aria-hidden className="inline-flex">{selected.icon}</span>}
        <input
          ref={inputRef}
          id={id}
          role="combobox"
          aria-expanded={open}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={open && shown[active] ? `${id}-opt-${shown[active].value}` : undefined}
          autoComplete="off"
          spellCheck={false}
          value={open ? query : (selected?.label ?? "")}
          placeholder={open || !selected ? placeholder : undefined}
          onFocus={() => {
            setOpen(true);
            setActive(Math.max(0, shown.findIndex((o) => o.value === value)));
          }}
          onBlur={close}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
            setOpen(true);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              if (!open) setOpen(true);
              else move(1);
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              move(-1);
            } else if (e.key === "Enter" && open) {
              e.preventDefault();
              pick(shown[active]);
            } else if (e.key === "Escape" && open) {
              e.preventDefault();
              e.stopPropagation();
              close();
            }
          }}
          className="min-w-0 flex-1 bg-transparent text-[13px] outline-none"
          style={{ color: "var(--ligus-ink)", border: 0, boxShadow: "none" }}
        />
        <span aria-hidden style={{ color: "var(--ligus-ink-soft)" }}>
          <IconChevronDown size={14} />
        </span>
      </div>
      {open && (
        <ul
          id={listId}
          role="listbox"
          aria-label={label}
          className="absolute left-0 right-0 z-[61] mt-1 max-h-64 overflow-y-auto rounded-xl border p-1"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", boxShadow: "var(--ligus-shadow-pop)" }}
          // Keep focus in the input while clicking an option.
          onMouseDown={(e) => e.preventDefault()}
        >
          {shown.length === 0 && (
            <li className="px-2.5 py-2 text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
              {emptyText}
            </li>
          )}
          {shown.map((o, i) => (
            <li
              key={o.value}
              id={`${id}-opt-${o.value}`}
              role="option"
              aria-selected={o.value === value}
              aria-disabled={o.disabled || undefined}
              onMouseEnter={() => !o.disabled && setActive(i)}
              onClick={() => pick(o)}
              className={`flex items-start gap-2 rounded-lg px-2.5 py-1.5 ${o.disabled ? "cursor-not-allowed" : "cursor-pointer"}`}
              style={{ background: i === active && !o.disabled ? "var(--ligus-overlay)" : undefined }}
            >
              {o.icon && <span aria-hidden className="mt-0.5 inline-flex">{o.icon}</span>}
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px]" style={{ color: o.disabled ? "var(--ligus-ink-faint)" : "var(--ligus-ink)" }}>
                  {o.label}
                </span>
                {(o.disabled ? o.disabledReason : o.hint) && (
                  <span className="block truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                    {o.disabled ? o.disabledReason : o.hint}
                  </span>
                )}
              </span>
              {o.value === value && (
                <span aria-hidden className="mt-0.5" style={{ color: "var(--ligus-accent-ink)" }}>
                  <IconCheck size={14} />
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
