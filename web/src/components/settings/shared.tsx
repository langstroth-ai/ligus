// Shared helpers for the split Settings sections. Inputs compose `fieldStyle`
// from ui.tsx (R2 — one field-style definition, no per-section copies).

import type { ReactNode } from "react";
import { InfoTip } from "@/components/ui";

/// Canonical class for every Settings text input / dropdown, so they line up.
/// Combine with a width class as needed: `${SELECT_CLASS} w-full`.
export const SELECT_CLASS = "rounded-xl border px-3 py-2 text-sm";

/// A settings field whose explanation sits behind an InfoTip beside the label
/// (web/README.md, Copy in panels) instead of a paragraph under the control.
/// Not a <label> wrapper: a button inside a label would steal its clicks, so
/// the control carries its own aria-label.
export function InfoField({ label, info, children }: { label: string; info?: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-1.5">
        <span className="text-[11.5px] font-semibold" style={{ color: "var(--ligus-ink-soft)" }}>
          {label}
        </span>
        {info && <InfoTip label={label} text={info} />}
      </div>
      {children}
    </div>
  );
}
