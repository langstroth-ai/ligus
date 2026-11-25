// Custom themes: a `ligus.theme.v1` file (TOML or JSON, the same file lt
// reads), imported in Settings → Appearance and stored on this device.
//
// The file is colours only. Every value goes through `parseColor`, a strict
// grammar (hex, rgb()/rgba(), oklch()), and what reaches the page is a
// canonical string rebuilt from the parsed numbers, set with
// style.setProperty on a --ligus-* property this module names itself. The
// file's own text never reaches CSS, and its keys never name a property.
// docs-site/docs/schemas/ligus-theme.v1.json is the schema;
// docs-site/docs/ligus/features/themes.md documents it.

import { useSyncExternalStore } from "react";
import { PALETTES, SCHEME_TOKENS } from "@/lib/tokens.generated";

export const THEME_FORMAT = "ligus.theme.v1";
/** Largest file accepted, checked before it is parsed. */
export const MAX_THEME_BYTES = 64 * 1024;
/** Longest colour value. */
export const MAX_VALUE_CHARS = 64;
export const MAX_NAME_CHARS = 40;
/** How many custom themes one device keeps. */
export const MAX_STORED_THEMES = 20;

export const BASE_PALETTES = ["pollen", "studio", "harbor", "meadow", "slate", "obsidian"] as const;
export type BasePalette = (typeof BASE_PALETTES)[number];
export type Variant = "light" | "dark";

/** Tokens the desktop draws with. */
export const PALETTE_TOKENS = [
  "canvas",
  "panel",
  "mist",
  "line",
  "ink",
  "accentCool",
  "accentWarm",
  "sidebarTop",
  "sidebarBottom",
  "sidebarInk",
  "sidebarInkMuted",
] as const;
export const SCHEME_TOKEN_KEYS = [
  "onAccent",
  "ok",
  "info",
  "warn",
  "danger",
  "neutral",
  "diffAddInk",
  "diffDelInk",
  "diffAddBg",
  "diffDelBg",
  "diffComment",
] as const;
/** Tokens only lt draws with: known, kept, unused here. */
export const LT_ONLY_TOKENS = ["inkMuted", "border", "selection", "agent"] as const;
export const DESKTOP_TOKENS = [...PALETTE_TOKENS, ...SCHEME_TOKEN_KEYS] as const;
export const ALL_TOKENS = [...DESKTOP_TOKENS, ...LT_ONLY_TOKENS] as const;
export type Token = (typeof ALL_TOKENS)[number];
type PaletteToken = (typeof PALETTE_TOKENS)[number];
type SchemeToken = (typeof SCHEME_TOKEN_KEYS)[number];

const TOKEN_SET: ReadonlySet<string> = new Set(ALL_TOKENS);

/** A colour token's value: canonical CSS rebuilt from parsed numbers. */
export type Colors = Partial<Record<Token, string>>;

export interface CustomTheme {
  id: string;
  name: string;
  extends: BasePalette;
  light: Colors;
  dark: Colors;
}

export type ImportResult =
  | { ok: true; theme: Omit<CustomTheme, "id">; warnings: string[] }
  | { ok: false; errors: string[]; warnings: string[] };

// ---- colours ---------------------------------------------------------------

export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

export type ParsedColor = { rgba: Rgba; css: string };

const NUM = String.raw`(?:\d+(?:\.\d+)?|\.\d+)`;
const HEX_RE = /^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const RGB_COMMA_RE = new RegExp(String.raw`^rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*(?:,\s*(${NUM})\s*)?\)$`, "i");
const RGB_SPACE_RE = new RegExp(String.raw`^rgba?\(\s*(\d{1,3})\s+(\d{1,3})\s+(\d{1,3})\s*(?:\/\s*(${NUM})\s*)?\)$`, "i");
const OKLCH_RE = new RegExp(String.raw`^oklch\(\s*(${NUM})(%?)\s+(${NUM})\s+(${NUM})\s*(?:\/\s*(${NUM})\s*)?\)$`, "i");

const fmt = (n: number, places: number) => String(Number(n.toFixed(places)));

const lin = (v: number) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const delin = (x: number) => 255 * (x <= 0.0031308 ? 12.92 * x : 1.055 * Math.sign(x) * Math.abs(x) ** (1 / 2.4) - 0.055);
const clip255 = (v: number) => Math.round(Math.min(255, Math.max(0, v)));

/** sRGB (0..255, clipped) of an OKLCH colour (Björn Ottosson's OKLab). */
export function oklchToRgb(L: number, C: number, H: number): [number, number, number] {
  const a = C * Math.cos((H * Math.PI) / 180);
  const b = C * Math.sin((H * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    clip255(delin(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s)),
    clip255(delin(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s)),
    clip255(delin(-0.0041960863 * l - 0.7034186147 * m + 1.707612701 * s)),
  ];
}

/**
 * Parse one colour value, strictly. Returns the colour and its canonical
 * CSS, or an error message. Anything outside the grammar is refused, so no
 * CSS other than a colour function this code writes can come out.
 */
export function parseColor(raw: unknown): ParsedColor | { error: string } {
  if (typeof raw !== "string") return { error: "must be a colour string" };
  if (raw.length > MAX_VALUE_CHARS) return { error: `is longer than ${MAX_VALUE_CHARS} characters` };
  const s = raw.trim();
  const bad = { error: `“${preview(raw)}” isn't a colour (use #rrggbb, rgb(), rgba() or oklch())` };
  if (HEX_RE.test(s)) {
    let h = s.slice(1);
    if (h.length === 3) h = [...h].map((c) => c + c).join("");
    const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
    const a = h.length === 8 ? n(6) / 255 : 1;
    return rgbResult(n(0), n(2), n(4), a);
  }
  let m = s.match(RGB_COMMA_RE) ?? s.match(RGB_SPACE_RE);
  if (m) {
    const [r, g, b] = [m[1], m[2], m[3]].map(Number);
    if ([r, g, b].some((v) => v > 255)) return { error: `“${preview(raw)}”: channels go from 0 to 255` };
    const a = m[4] === undefined ? 1 : Number(m[4]);
    if (a > 1) return { error: `“${preview(raw)}”: alpha goes from 0 to 1` };
    return rgbResult(r, g, b, a);
  }
  m = s.match(OKLCH_RE);
  if (m) {
    const L = Number(m[1]) / (m[2] ? 100 : 1);
    const C = Number(m[3]);
    const H = Number(m[4]);
    const a = m[5] === undefined ? 1 : Number(m[5]);
    if (L > 1) return { error: `“${preview(raw)}”: lightness goes from 0 to 1 (or 0% to 100%)` };
    if (C > 0.5) return { error: `“${preview(raw)}”: chroma goes from 0 to 0.5` };
    if (H > 360) return { error: `“${preview(raw)}”: hue goes from 0 to 360` };
    if (a > 1) return { error: `“${preview(raw)}”: alpha goes from 0 to 1` };
    // Applied as rgb(): the editor, the terminal and the title bar read
    // these tokens back and understand rgb() and hex only.
    const clip = (v: number) => Math.round(Math.min(255, Math.max(0, v)));
    const [r, g, b] = oklchToRgb(L, C, H).map(clip);
    return rgbResult(r, g, b, a);
  }
  return bad;
}

function rgbResult(r: number, g: number, b: number, a: number): ParsedColor {
  return { rgba: { r, g, b, a }, css: a >= 1 ? `rgb(${r},${g},${b})` : `rgba(${r},${g},${b},${fmt(a, 3)})` };
}

/** A value for a message: printable, short. */
function preview(s: string): string {
  // eslint-disable-next-line no-control-regex
  const clean = s.replace(/[\u0000-\u001f\u007f]/g, " ");
  return clean.length > 32 ? clean.slice(0, 32) + "…" : clean;
}

/** An opaque colour painted over an opaque background. */
function over(fg: Rgba, bg: Rgba): [number, number, number] {
  return [fg.r * fg.a + bg.r * (1 - fg.a), fg.g * fg.a + bg.g * (1 - fg.a), fg.b * fg.a + bg.b * (1 - fg.a)];
}

/** WCAG 2.x contrast ratio of two opaque colours. */
export function contrast(a: [number, number, number], b: [number, number, number]): number {
  const lum = ([r, g, b]: [number, number, number]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  const [x, y] = [lum(a), lum(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

// ---- reading the file ------------------------------------------------------

/** Key → value, with the line it came from (TOML; JSON finds it). */
type Entry = { value: unknown; line?: number };
type Table = Map<string, Entry>;
type Doc = { top: Table; tables: Map<string, Table> };

class FileError extends Error {}

/**
 * A strict reader for the TOML this format needs: `key = "string"` pairs,
 * `[light]` and `[dark]` tables, dotted keys one deep (`light.ink = "…"`),
 * basic and literal strings, comments. Anything else (numbers, arrays,
 * inline tables, multi-line strings, nested tables) is an error with its
 * line number, and so is a duplicate key or table.
 */
export function readToml(text: string): Doc {
  const top: Table = new Map();
  const tables = new Map<string, Table>();
  let current: Table = top;
  const lines = text.split(/\r?\n/);
  lines.forEach((rawLine, i) => {
    const n = i + 1;
    const line = rawLine.trim();
    if (line === "" || line.startsWith("#")) return;
    if (line.startsWith("[")) {
      const m = line.match(/^\[\s*([A-Za-z0-9_-]+)\s*\]\s*(?:#.*)?$/);
      if (!m) throw new FileError(`line ${n}: only [light] and [dark] tables are allowed`);
      const name = m[1];
      if (tables.has(name)) throw new FileError(`line ${n}: [${name}] appears twice`);
      if (top.has(name)) throw new FileError(`line ${n}: ${name} is already set`);
      current = new Map();
      tables.set(name, current);
      return;
    }
    let rest = line;
    const keys: string[] = [];
    for (;;) {
      const k = readKey(rest, n);
      keys.push(k.key);
      rest = k.rest.trimStart();
      if (rest.startsWith(".")) {
        rest = rest.slice(1).trimStart();
        continue;
      }
      break;
    }
    if (!rest.startsWith("=")) throw new FileError(`line ${n}: expected key = "value"`);
    rest = rest.slice(1).trim();
    const { value, after } = readString(rest, n, keys.join("."));
    if (after.trim() !== "" && !after.trim().startsWith("#")) throw new FileError(`line ${n}: unexpected text after the value`);
    let target = current;
    let key = keys[0];
    if (keys.length === 2 && current === top) {
      if (top.has(keys[0])) throw new FileError(`line ${n}: ${keys[0]} is already set`);
      const t = tables.get(keys[0]) ?? new Map();
      tables.set(keys[0], t);
      target = t;
      key = keys[1];
    } else if (keys.length !== 1) {
      throw new FileError(`line ${n}: ${keys.join(".")}: tables go one level deep ([light] and [dark])`);
    }
    if (target === top && tables.has(key)) throw new FileError(`line ${n}: ${key} is already a table`);
    if (target.has(key)) throw new FileError(`line ${n}: ${keys.join(".")} is set twice`);
    target.set(key, { value, line: n });
  });
  return { top, tables };
}

function readKey(s: string, n: number): { key: string; rest: string } {
  if (s.startsWith('"') || s.startsWith("'")) {
    const { value, after } = readString(s, n, "a key");
    return { key: value, rest: after };
  }
  const m = s.match(/^[A-Za-z0-9_-]+/);
  if (!m) throw new FileError(`line ${n}: expected a key`);
  return { key: m[0], rest: s.slice(m[0].length) };
}

function readString(s: string, n: number, what: string): { value: string; after: string } {
  if (s.startsWith('"""') || s.startsWith("'''")) throw new FileError(`line ${n}: ${what}: multi-line strings aren't supported`);
  if (s.startsWith("'")) {
    const end = s.indexOf("'", 1);
    if (end < 0) throw new FileError(`line ${n}: ${what}: the string isn't closed`);
    return { value: s.slice(1, end), after: s.slice(end + 1) };
  }
  if (!s.startsWith('"')) throw new FileError(`line ${n}: ${what} must be a string in quotes`);
  let out = "";
  for (let i = 1; i < s.length; i++) {
    const c = s[i];
    if (c === '"') return { value: out, after: s.slice(i + 1) };
    if (c === "\\") {
      const e = s[++i];
      const simple: Record<string, string> = { b: "\b", t: "\t", n: "\n", f: "\f", r: "\r", '"': '"', "\\": "\\" };
      if (e !== undefined && Object.hasOwn(simple, e)) {
        out += simple[e];
      } else if (e === "u" || e === "U") {
        const len = e === "u" ? 4 : 8;
        const hex = s.slice(i + 1, i + 1 + len);
        if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== len) throw new FileError(`line ${n}: ${what}: bad \\${e} escape`);
        const cp = parseInt(hex, 16);
        if (cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) throw new FileError(`line ${n}: ${what}: bad \\${e} escape`);
        out += String.fromCodePoint(cp);
        i += len;
      } else {
        throw new FileError(`line ${n}: ${what}: bad escape`);
      }
      continue;
    }
    out += c;
  }
  throw new FileError(`line ${n}: ${what}: the string isn't closed`);
}

/** JSON into the same shape; lines are found by searching for the key. */
export function readJson(text: string): Doc {
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch (e) {
    throw new FileError(`not valid JSON: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (!isPlainObject(data)) throw new FileError("the file must be a JSON object");
  // Line starts, once, so each lookup is a binary search (a 64 KB file of
  // short lines stays fast).
  const starts = [0];
  for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) starts.push(i + 1);
  const lineAt = (at: number) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= at) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
  const lineOf = (key: string, from = 0) => {
    const at = text.indexOf(JSON.stringify(key), from);
    return at < 0 ? undefined : lineAt(at);
  };
  const top: Table = new Map();
  const tables = new Map<string, Table>();
  for (const key of Object.keys(data)) {
    const value = (data as Record<string, unknown>)[key];
    if ((key === "light" || key === "dark") && isPlainObject(value)) {
      const t: Table = new Map();
      // The table's own key (a key followed by a colon), not an earlier
      // mention of the word in a string.
      const m = new RegExp(`${JSON.stringify(key)}\\s*:`).exec(text);
      const from = m ? m.index : 0;
      for (const k of Object.keys(value)) t.set(k, { value: (value as Record<string, unknown>)[k], line: lineOf(k, from) });
      tables.set(key, t);
    } else {
      top.set(key, { value, line: lineOf(key) });
    }
  }
  return { top, tables };
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

const at = (e: Entry) => (e.line ? `line ${e.line}: ` : "");

/**
 * Read and check a theme file. Errors refuse it; warnings (unknown keys,
 * low contrast) don't.
 */
export function parseThemeFile(text: string, fileName = "theme"): ImportResult {
  const errors: string[] = [];
  const warnings: string[] = [];
  if (typeof text !== "string") return { ok: false, errors: ["the file isn't text"], warnings };
  if (new TextEncoder().encode(text).length > MAX_THEME_BYTES) {
    return { ok: false, errors: [`the file is larger than ${MAX_THEME_BYTES / 1024} KB`], warnings };
  }
  const body = text.replace(/^﻿/, "");
  let doc: Doc;
  try {
    doc = body.trimStart().startsWith("{") ? readJson(body) : readToml(body);
  } catch (e) {
    return { ok: false, errors: [e instanceof FileError ? e.message : String(e)], warnings };
  }

  let name = nameFromFile(fileName);
  let base: BasePalette = "pollen";
  for (const [key, entry] of doc.top) {
    const v = entry.value;
    switch (key) {
      case "$schema":
        if (typeof v !== "string") errors.push(`${at(entry)}$schema must be a string`);
        break;
      case "format":
        if (v !== THEME_FORMAT) errors.push(`${at(entry)}format must be "${THEME_FORMAT}"`);
        break;
      case "name": {
        const checked = checkName(v);
        if (typeof checked === "string") name = checked;
        else errors.push(`${at(entry)}name ${checked.error}`);
        break;
      }
      case "extends":
        if (typeof v === "string" && (BASE_PALETTES as readonly string[]).includes(v)) base = v as BasePalette;
        else errors.push(`${at(entry)}extends must be one of ${BASE_PALETTES.join(", ")}`);
        break;
      case "light":
      case "dark":
        errors.push(`${at(entry)}${key} must be a table of colours`);
        break;
      default:
        warnings.push(`${at(entry)}unknown key ${printable(key)} (ignored)`);
    }
  }
  for (const key of doc.tables.keys()) {
    if (key !== "light" && key !== "dark") {
      const first = doc.tables.get(key)!.values().next().value as Entry | undefined;
      warnings.push(`${first ? at(first) : ""}unknown table ${printable(key)} (ignored)`);
    }
  }
  const out: Record<Variant, Colors> = { light: {}, dark: {} };
  const rgba: Record<Variant, Partial<Record<Token, Rgba>>> = { light: {}, dark: {} };
  if (!doc.tables.has("light") && !doc.tables.has("dark")) errors.push("the file needs a [light] or a [dark] table");
  for (const variant of ["light", "dark"] as const) {
    const t = doc.tables.get(variant);
    if (!t) continue;
    for (const [key, entry] of t) {
      if (!TOKEN_SET.has(key)) {
        warnings.push(`${at(entry)}unknown colour ${variant}.${printable(key)} (ignored)`);
        continue;
      }
      const c = parseColor(entry.value);
      if ("error" in c) errors.push(`${at(entry)}${variant}.${key} ${c.error}`);
      else {
        out[variant][key as Token] = c.css;
        rgba[variant][key as Token] = c.rgba;
      }
    }
  }
  if (errors.length) return { ok: false, errors, warnings };
  const theme = { name, extends: base, light: out.light, dark: out.dark };
  warnings.push(...contrastWarnings(theme));
  return { ok: true, theme, warnings };
}

function printable(s: string): string {
  return JSON.stringify(preview(s));
}

function checkName(v: unknown): string | { error: string } {
  if (typeof v !== "string") return { error: "must be a string" };
  const s = v.trim();
  if (!s) return { error: "is empty" };
  if (s.length > MAX_NAME_CHARS) return { error: `is longer than ${MAX_NAME_CHARS} characters` };
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f\u2028\u2029]/.test(s)) return { error: "has control characters" };
  if (/[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/.test(s)) return { error: "has bidi control characters" };
  // A lone surrogate couldn't be written back to a file (export, lt).
  if (/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/.test(s)) return { error: "isn't valid text" };
  if (!s.replace(/[\u200b-\u200d\ufeff]/g, "").trim()) return { error: "is empty" };
  return s;
}

/** A display name from a file name: its stem, or "Custom theme". */
export function nameFromFile(fileName: string): string {
  const stem = fileName.replace(/^.*[\\/]/, "").replace(/\.(toml|json)$/i, "");
  const checked = checkName(stem);
  return typeof checked === "string" ? checked : "Custom theme";
}

// ---- resolving -------------------------------------------------------------

type SchemeValues = (typeof SCHEME_TOKENS)["light"];
/** Desktop scheme tokens and the CSS custom property each sets. */
const SCHEME_CSS: Record<SchemeToken, { prop: string; from: keyof SchemeValues }> = {
  onAccent: { prop: "--ligus-on-accent", from: "onAccent" },
  ok: { prop: "--ligus-success", from: "success" },
  info: { prop: "--ligus-info", from: "info" },
  warn: { prop: "--ligus-warn", from: "warn" },
  danger: { prop: "--ligus-danger", from: "danger" },
  neutral: { prop: "--ligus-neutral", from: "neutral" },
  diffAddInk: { prop: "--ligus-diff-add-ink", from: "diffAddInk" },
  diffDelInk: { prop: "--ligus-diff-del-ink", from: "diffDelInk" },
  diffAddBg: { prop: "--ligus-diff-add-bg", from: "diffAddBg" },
  diffDelBg: { prop: "--ligus-diff-del-bg", from: "diffDelBg" },
  diffComment: { prop: "--ligus-diff-comment", from: "diffComment" },
};

export interface ResolvedPalette {
  canvas: string;
  ink: string;
  panel: string;
  mist: string;
  line: string;
  accentWarm: string;
  accentCool: string;
  sidebarTop: string;
  sidebarBottom: string;
  sidebarInk: string;
  sidebarInkMuted: string;
  scheme: "light" | "dark";
}

/** A custom theme for one appearance: the palette, and the scheme tokens it sets. */
export function resolveCustom(t: Pick<CustomTheme, "extends" | "light" | "dark">, variant: Variant) {
  const base = PALETTES[t.extends][variant];
  const own = t[variant];
  const palette: ResolvedPalette = { ...base };
  for (const k of PALETTE_TOKENS) {
    const v = own[k];
    if (v !== undefined) palette[k as PaletteToken] = v;
  }
  const scheme: Record<string, string> = {};
  for (const k of SCHEME_TOKEN_KEYS) {
    const v = own[k];
    if (v !== undefined) scheme[SCHEME_CSS[k].prop] = v;
  }
  return { palette, scheme };
}

/**
 * Low-contrast pairs in a theme, as warnings. Only pairs that involve a
 * colour the file sets: the built-in palettes are checked elsewhere.
 */
export function contrastWarnings(t: Pick<CustomTheme, "extends" | "light" | "dark">): string[] {
  const out: string[] = [];
  for (const variant of ["light", "dark"] as const) {
    const own = t[variant];
    if (!Object.keys(own).length) continue;
    const scheme = PALETTES[t.extends][variant].scheme;
    const value = (k: Token): string | undefined => {
      if (own[k] !== undefined) return own[k];
      if ((PALETTE_TOKENS as readonly string[]).includes(k)) return PALETTES[t.extends][variant][k as PaletteToken];
      if ((SCHEME_TOKEN_KEYS as readonly string[]).includes(k)) return SCHEME_TOKENS[scheme][SCHEME_CSS[k as SchemeToken].from] as string;
      return undefined;
    };
    const solid = (k: Token, bg: [number, number, number]): [number, number, number] | null => {
      const v = value(k);
      if (v === undefined) return null;
      const c = parseColor(v);
      return "error" in c ? null : over(c.rgba, { r: bg[0], g: bg[1], b: bg[2], a: 1 });
    };
    const white: [number, number, number] = scheme === "dark" ? [0, 0, 0] : [255, 255, 255];
    const canvas = solid("canvas", white);
    const panel = solid("panel", white);
    if (!canvas || !panel) continue;
    const check = (fg: Token, bgKey: Token, bg: [number, number, number], min: number) => {
      if (own[fg] === undefined && own[bgKey] === undefined) return;
      const c = solid(fg, bg);
      if (!c) return;
      const ratio = contrast(c, bg);
      if (ratio < min) out.push(`${variant}: ${fg} on ${bgKey} is ${ratio.toFixed(2)}:1 (aim for ${min}:1)`);
    };
    check("ink", "canvas", canvas, 4.5);
    check("ink", "panel", panel, 4.5);
    for (const k of ["accentCool", "accentWarm", "ok", "info", "warn", "danger", "neutral"] as const) check(k, "canvas", canvas, 3);
    const accent = solid("accentCool", canvas);
    if (accent) check("onAccent", "accentCool", accent, 4.5);
  }
  return out;
}

/** Set a resolved custom theme's scheme tokens on :root (after applyTheme). */
export function applySchemeOverrides(scheme: Record<string, string>) {
  const root = document.documentElement;
  for (const [prop, value] of Object.entries(scheme)) {
    // Both come from this module: a SCHEME_CSS property and a canonical colour.
    if (!prop.startsWith("--ligus-")) continue;
    root.style.setProperty(prop, value);
  }
}

// ---- writing a file --------------------------------------------------------

/** A full theme file (TOML) for one palette or custom theme: every desktop token, light and dark. */
export function exportThemeToml(t: Pick<CustomTheme, "name" | "extends" | "light" | "dark">): string {
  const q = (s: string) => JSON.stringify(s);
  const lines = [
    `# A Ligus theme (${THEME_FORMAT}). The desktop app and lt both read it.`,
    `# Colours only: #rrggbb, rgb(), rgba() or oklch(). Tokens you leave out come from "extends".`,
    `format = ${q(THEME_FORMAT)}`,
    `name = ${q(t.name)}`,
    `extends = ${q(t.extends)}`,
  ];
  for (const variant of ["light", "dark"] as const) {
    const values = fullColors(t, variant);
    lines.push("", `[${variant}]`);
    for (const k of ALL_TOKENS) {
      const v = values[k];
      if (v !== undefined) lines.push(`${k} = ${q(v)}`);
    }
  }
  return lines.join("\n") + "\n";
}

/** The same as JSON. */
export function exportThemeJson(t: Pick<CustomTheme, "name" | "extends" | "light" | "dark">): string {
  const obj: Record<string, unknown> = { format: THEME_FORMAT, name: t.name, extends: t.extends };
  for (const variant of ["light", "dark"] as const) obj[variant] = fullColors(t, variant);
  return JSON.stringify(obj, null, 2) + "\n";
}

function fullColors(t: Pick<CustomTheme, "extends" | "light" | "dark">, variant: Variant): Colors {
  const p = PALETTES[t.extends][variant];
  const s = SCHEME_TOKENS[p.scheme];
  const out: Colors = {};
  for (const k of PALETTE_TOKENS) out[k] = p[k];
  for (const k of SCHEME_TOKEN_KEYS) out[k] = s[SCHEME_CSS[k].from] as string;
  return { ...out, ...t[variant] };
}

/** A safe file name for an export. */
export function exportFileName(name: string): string {
  const stem = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return `${stem || "ligus-theme"}.toml`;
}

// ---- storage (this device) -------------------------------------------------

export const STORAGE_KEY = "ligus.customThemes";
export const ACTIVE_KEY = "ligus.customTheme";
/** A stored list larger than this is dropped unread. */
const MAX_STORED_CHARS = MAX_STORED_THEMES * 8 * 1024;

/** Check a stored theme as strictly as an imported one; null if it fails. */
function revalidate(raw: unknown): CustomTheme | null {
  if (!isPlainObject(raw)) return null;
  const { id, name, extends: base, light, dark } = raw as Record<string, unknown>;
  if (typeof id !== "string" || !/^[a-z0-9-]{1,40}$/.test(id)) return null;
  const n = checkName(name);
  if (typeof n !== "string") return null;
  if (typeof base !== "string" || !(BASE_PALETTES as readonly string[]).includes(base)) return null;
  const colors = (v: unknown): Colors | null => {
    if (!isPlainObject(v)) return null;
    const out: Colors = {};
    for (const k of Object.keys(v)) {
      if (!TOKEN_SET.has(k)) return null;
      const c = parseColor((v as Record<string, unknown>)[k]);
      if ("error" in c) return null;
      out[k as Token] = c.css;
    }
    return out;
  };
  const l = colors(light);
  const d = colors(dark);
  if (!l || !d) return null;
  return { id, name: n, extends: base as BasePalette, light: l, dark: d };
}

function readStore(): CustomTheme[] {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(STORAGE_KEY);
  } catch {
    return [];
  }
  if (!raw || raw.length > MAX_STORED_CHARS) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!isPlainObject(parsed) || parsed.v !== 1 || !Array.isArray(parsed.themes)) return [];
    const seen = new Set<string>();
    const out: CustomTheme[] = [];
    for (const t of parsed.themes.slice(0, MAX_STORED_THEMES)) {
      const ok = revalidate(t);
      if (ok && !seen.has(ok.id)) {
        seen.add(ok.id);
        out.push(ok);
      }
    }
    return out;
  } catch {
    return [];
  }
}

function writeStore(themes: CustomTheme[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ v: 1, themes }));
  } catch {
    // Storage full or blocked: the change lasts this session only.
  }
}

type Snapshot = { themes: CustomTheme[]; active: CustomTheme | null };
let snapshot: Snapshot | null = null;
const listeners = new Set<() => void>();

function load(): Snapshot {
  const themes = readStore();
  let activeId: string | null = null;
  try {
    activeId = localStorage.getItem(ACTIVE_KEY);
  } catch {
    activeId = null;
  }
  return { themes, active: themes.find((t) => t.id === activeId) ?? null };
}

function commit(themes: CustomTheme[], activeId: string | null) {
  writeStore(themes);
  try {
    if (activeId) localStorage.setItem(ACTIVE_KEY, activeId);
    else localStorage.removeItem(ACTIVE_KEY);
  } catch {
    // as above
  }
  snapshot = { themes, active: themes.find((t) => t.id === activeId) ?? null };
  listeners.forEach((l) => l());
}

/** This device's custom themes and the one in use (if any). */
export function customThemes(): Snapshot {
  if (!snapshot) snapshot = load();
  return snapshot;
}

/** Re-read storage (tests, or after another window changed it). */
export function reloadCustomThemes() {
  snapshot = load();
  listeners.forEach((l) => l());
}

export function subscribeCustomThemes(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

export function useCustomThemes(): Snapshot {
  return useSyncExternalStore(subscribeCustomThemes, customThemes, customThemes);
}

function newId(): string {
  const r = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`;
  return r.toLowerCase().replace(/[^a-z0-9-]/g, "").slice(0, 36);
}

/** Keep an imported theme and use it. Throws when the device already keeps the most. */
export function addCustomTheme(t: Omit<CustomTheme, "id">): CustomTheme {
  const { themes } = customThemes();
  if (themes.length >= MAX_STORED_THEMES) throw new Error(`This device keeps ${MAX_STORED_THEMES} themes. Remove one first.`);
  const theme = { ...t, id: newId() };
  commit([...themes, theme], theme.id);
  return theme;
}

export function renameCustomTheme(id: string, name: string): boolean {
  const n = checkName(name);
  if (typeof n !== "string") return false;
  const { themes, active } = customThemes();
  commit(
    themes.map((t) => (t.id === id ? { ...t, name: n } : t)),
    active?.id ?? null,
  );
  return true;
}

export function removeCustomTheme(id: string) {
  const { themes, active } = customThemes();
  commit(
    themes.filter((t) => t.id !== id),
    active?.id === id ? null : (active?.id ?? null),
  );
}

/** Use a custom theme, or none (a built-in palette). */
export function setActiveCustomTheme(id: string | null) {
  const { themes } = customThemes();
  commit(themes, id && themes.some((t) => t.id === id) ? id : null);
}
