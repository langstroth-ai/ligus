// A fingerprint to compare by eye: the characters in groups of four, in a
// monospace face with tabular figures, wrapping rather than shrinking. Screen
// readers hear it spelled one character at a time, grouped. A copy button
// saves anyone from transcribing it.

import { useState } from "react";
import { IconCheck, IconCopy } from "@/lib/icons";

/// Split into groups of `size` (spaces and dashes in the input are ignored).
export function groupFingerprint(value: string, size = 4): string[] {
  const clean = value.replace(/[\s-]/g, "").toUpperCase();
  const out: string[] = [];
  for (let i = 0; i < clean.length; i += size) out.push(clean.slice(i, i + size));
  return out;
}

/// "AB12 CD" → "A B 1 2, C D": what a screen reader should say.
export function spellFingerprint(groups: string[]): string {
  return groups.map((g) => g.split("").join(" ")).join(", ");
}

/// A code-match code ("K7Q4 M9X2", Crockford base32) as its two groups:
/// upper case, spaces and dashes ignored, I and L read as 1 and O as 0.
export function groupMatchCode(code: string): string[] {
  const clean = code
    .replace(/[\s-]/g, "")
    .toUpperCase()
    .replace(/[IL]/g, "1")
    .replace(/O/g, "0");
  return [clean.slice(0, 4), clean.slice(4, 8)].filter(Boolean);
}

/// "K7Q4 M9X2" → "K, 7, Q, 4. M, 9, X, 2.": letter by letter, in groups.
export function spellMatchCode(code: string): string {
  return groupMatchCode(code)
    .map((g) => `${g.split("").join(", ")}.`)
    .join(" ");
}

export function Fingerprint({
  value,
  label = "Fingerprint",
  size = "md",
  copyable = true,
}: {
  value: string;
  /// What it is, for screen readers and the copy button ("Device fingerprint").
  label?: string;
  size?: "md" | "lg";
  copyable?: boolean;
}) {
  const groups = groupFingerprint(value);
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard
      ?.writeText(groups.join(" "))
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {});
  };
  return (
    <div className="flex items-start gap-2">
      <p
        role="img"
        aria-label={`${label}: ${spellFingerprint(groups)}`}
        className={`flex min-w-0 flex-1 flex-wrap gap-x-2.5 gap-y-1 tabular-nums ${size === "lg" ? "text-[17px] leading-7" : "text-[13px] leading-5"}`}
        style={{ fontFamily: "var(--ligus-mono)", color: "var(--ligus-ink)", letterSpacing: "0.04em" }}
      >
        {groups.map((g, i) => (
          <span key={i} aria-hidden>
            {g}
          </span>
        ))}
      </p>
      {copyable && (
        <button
          type="button"
          onClick={copy}
          aria-label={copied ? `${label} copied` : `Copy ${label.toLowerCase()}`}
          className="ligus-press inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-lg"
          style={{ color: copied ? "var(--ligus-success-ink)" : "var(--ligus-ink-soft)" }}
        >
          {copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
        </button>
      )}
    </div>
  );
}
