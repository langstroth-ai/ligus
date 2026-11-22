import { describe, expect, it } from "vitest";

// Static checks for the type rules: emphasis comes from weight and ink-soft,
// never an opacity fade; no text below 11px; one heading treatment per level.
const sources = import.meta.glob(["../components/**/*.tsx", "../App.tsx"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const css = import.meta.glob("../styles.css", { query: "?raw", import: "default", eager: true }) as Record<string, string>;

// Files still being changed elsewhere; they move to the rule when that lands.
const PENDING = ["/Sidebar.tsx", "/settings/ModelsSection.tsx"];
const files = Object.entries(sources).filter(([p]) => !p.includes(".test.") && !PENDING.some((x) => p.endsWith(x)));

describe("type rules", () => {
  it("finds the sources", () => {
    expect(files.length).toBeGreaterThan(50);
  });

  it("uses ink-soft, not opacity fades, for quieter text", () => {
    const offenders: string[] = [];
    for (const [path, src] of files) {
      src.split("\n").forEach((line, i) => {
        // Decorative dots (bg-current) may fade; reveal-on-hover (opacity-0)
        // and disabled states are not emphasis.
        if (line.includes("bg-current")) return;
        for (const m of line.matchAll(/(?<![\w:\-/[])opacity-(\d+)/g)) {
          const n = Number(m[1]);
          if (n !== 0 && n !== 100) offenders.push(`${path}:${i + 1}: ${line.trim().slice(0, 100)}`);
        }
      });
    }
    expect(offenders).toEqual([]);
  });

  it("has no text below 11px", () => {
    const offenders: string[] = [];
    for (const [path, src] of files) {
      for (const m of src.matchAll(/text-\[(\d+(?:\.\d+)?)px\]|fontSize:\s*(\d+(?:\.\d+)?)[,\s}]/g)) {
        const px = Number(m[1] ?? m[2]);
        if (px < 11) offenders.push(`${path}: ${m[0]}`);
      }
    }
    for (const [path, src] of Object.entries(css)) {
      for (const m of src.matchAll(/font-size:\s*(\d+(?:\.\d+)?)px/g)) {
        if (Number(m[1]) < 11) offenders.push(`${path}: ${m[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("titles headings with the shared heading classes", () => {
    const offenders: string[] = [];
    for (const [path, src] of files) {
      for (const m of src.matchAll(/<h([1-3])\s+className=\{?["`]([^"`]*)["`]/g)) {
        if (!/\bligus-(display|title|section|cap)\b/.test(m[2])) offenders.push(`${path}: <h${m[1]} ${m[2].slice(0, 60)}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
