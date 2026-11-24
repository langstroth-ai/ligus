// Design-token palettes for the React shell, applied to :root as CSS custom
// properties. Each theme is an accent *family* with a light and a dark variant;
// the appearance mode (auto/light/dark) picks the variant. `pollen` (Ligus's
// warm honey-gold identity) is the launch default.
//
//  pollen   — honey / gold (brand)
//  studio   — neutral graphite (achromatic, professional)
//  harbor   — cool ocean blue
//  slate    — cool blue-grey
//  meadow   — botanical green
//  obsidian — true dark, monochrome

import { useSyncExternalStore } from "react";
import { setTitlebarColor } from "@/lib/ipc";
import { PALETTES, SCHEME_TOKENS } from "@/lib/tokens.generated";
import { applySchemeOverrides, resolveCustom, type CustomTheme } from "@/lib/customThemes";

export type ThemeName = "pollen" | "studio" | "harbor" | "meadow" | "slate" | "obsidian";
export type AppearanceMode = "auto" | "light" | "dark";

export interface Palette {
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

interface ThemeVariants {
  light: Palette;
  dark: Palette;
}

// The palettes are authored in OKLCH in assets/branding/ligus-brand-tokens.json
// (one source for the desktop, the phone and lt); tokens.generated.ts holds
// them as the rgb() values applied here. Edit the JSON, then run
// `node assets/branding/tokens/generate.mjs`.
//
// Obsidian is a true dark, monochrome theme: both variants are the same
// neutral near-black with silver-grey accents, and `scheme: "dark"` in both
// keeps the dark status and chat tokens whatever the Appearance toggle says.
export const THEMES: Record<ThemeName, ThemeVariants> = PALETTES;

const STORAGE_KEY = "ligus.theme";
const MODE_KEY = "ligus.appearance";
const DEFAULT_THEME: ThemeName = "pollen";

/// The user's preferred accent family (independent of light/dark mode).
export function loadTheme(): ThemeName {
  const stored = localStorage.getItem(STORAGE_KEY);
  // Migration: the cool-blue theme was renamed midnight → slate (the name now
  // belongs conceptually to the warm Obsidian dark). Remap a saved value.
  if (stored === "midnight") return "slate";
  if (stored && stored in THEMES) return stored as ThemeName;
  return DEFAULT_THEME;
}

export function savePalette(name: ThemeName) {
  localStorage.setItem(STORAGE_KEY, name);
}

export function loadMode(): AppearanceMode {
  const m = localStorage.getItem(MODE_KEY);
  return m === "light" || m === "dark" || m === "auto" ? m : "auto";
}

export function saveMode(mode: AppearanceMode) {
  localStorage.setItem(MODE_KEY, mode);
}

export function systemPrefersDark(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
  );
}

/// Run `cb` whenever the OS light/dark preference flips. Returns an unsubscribe.
export function watchSystemScheme(cb: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  mq.addEventListener("change", cb);
  return () => mq.removeEventListener("change", cb);
}

/// Resolve the concrete palette to render given the chosen family + mode.
export function resolvePalette(name: ThemeName, mode: AppearanceMode): Palette {
  const wantsDark = mode === "dark" || (mode === "auto" && systemPrefersDark());
  const family = THEMES[name] ?? THEMES[DEFAULT_THEME];
  return wantsDark ? family.dark : family.light;
}

/// Apply what Settings chose: a custom theme (imported on this device) when
/// one is in use, else the built-in palette. Both follow the mode the same way.
export function applyActiveTheme(name: ThemeName, mode: AppearanceMode, custom: CustomTheme | null) {
  if (!custom) {
    applyTheme(resolvePalette(name, mode));
    return;
  }
  const wantsDark = mode === "dark" || (mode === "auto" && systemPrefersDark());
  const { palette, scheme } = resolveCustom(custom, wantsDark ? "dark" : "light");
  applyTheme(palette, scheme);
}

/// Status + overlay tokens per scheme (identical across accent families): one
/// legible green/red/amber per light/dark rather than eight hand-tuned variants.
/// Warn is an amber-orange (OKLCH hue 45 in light, 47 in dark) kept clearly
/// apart from the honey brand accent (hue 75-85), from danger's red and from
/// every palette's accent-warm; tokens/validate.mjs checks the distances,
/// including under simulated colour-vision deficiencies, and contrast.
export const STATUS_COLORS: Record<"light" | "dark", { success: string; danger: string; warn: string; info: string; neutral: string; overlay: string }> = {
  light: pickStatus("light"),
  dark: pickStatus("dark"),
};

function pickStatus(scheme: "light" | "dark") {
  const t = SCHEME_TOKENS[scheme];
  return { success: t.success, danger: t.danger, warn: t.warn, info: t.info, neutral: t.neutral, overlay: t.overlay };
}

export function applyTheme(p: Palette, schemeOverrides?: Record<string, string>) {
  const root = document.documentElement;
  // Status + overlay tokens are scheme-derived (identical across accent
  // families) — one legible green/red/amber per light/dark rather than eight
  // hand-tuned variants. Components must use these instead of hardcoded
  // #34c759-style literals so light palettes stay readable.
  const t = SCHEME_TOKENS[p.scheme];
  root.style.setProperty("--ligus-success", t.success);
  root.style.setProperty("--ligus-danger", t.danger);
  root.style.setProperty("--ligus-warn", t.warn);
  root.style.setProperty("--ligus-info", t.info);
  root.style.setProperty("--ligus-neutral", t.neutral);
  root.style.setProperty("--ligus-overlay", t.overlay);
  root.style.setProperty("--ligus-diff-add-bg", t.diffAddBg);
  root.style.setProperty("--ligus-diff-add-ink", t.diffAddInk);
  root.style.setProperty("--ligus-diff-del-bg", t.diffDelBg);
  root.style.setProperty("--ligus-diff-del-ink", t.diffDelInk);
  root.style.setProperty("--ligus-diff-comment", t.diffComment);
  t.presence.forEach((c, i) => root.style.setProperty(`--ligus-presence-${i + 1}`, c));
  // Chat-layer scheme-derived numbers (redesign). Like the status tokens they
  // depend on light/dark, never on the accent family — so keying them to a
  // theme name would be wrong. They set the target lightness for turn fills,
  // rails, tinted author names, and filled controls, plus the on-accent text
  // colour, so one CSS rule set stays legible in every palette + scheme.
  root.style.setProperty("--ligus-turn-mix", t.turnMix);
  root.style.setProperty("--ligus-turn-rail-l", t.turnRailL);
  root.style.setProperty("--ligus-turn-name-l", t.turnNameL);
  root.style.setProperty("--ligus-accent-ink-l", t.accentInkL);
  root.style.setProperty("--ligus-fill-l", t.fillL);
  root.style.setProperty("--ligus-on-accent", t.onAccent);
  root.style.setProperty("--ligus-canvas", p.canvas);
  root.style.setProperty("--ligus-ink", p.ink);
  root.style.setProperty("--ligus-panel", p.panel);
  root.style.setProperty("--ligus-mist", p.mist);
  root.style.setProperty("--ligus-line", p.line);
  root.style.setProperty("--ligus-accent-warm", p.accentWarm);
  root.style.setProperty("--ligus-accent-cool", p.accentCool);
  root.style.setProperty("--ligus-sidebar-top", p.sidebarTop);
  root.style.setProperty("--ligus-sidebar-bottom", p.sidebarBottom);
  root.style.setProperty("--ligus-sidebar-ink", p.sidebarInk);
  root.style.setProperty("--ligus-sidebar-ink-muted", p.sidebarInkMuted);
  root.style.colorScheme = p.scheme;
  // A custom theme's own status, diff and on-accent colours (validated,
  // canonical values for fixed --ligus-* properties: lib/customThemes.ts).
  if (schemeOverrides) applySchemeOverrides(schemeOverrides);
  // Let scheme-aware embeds (Monaco diff, xterm logs) react to theme flips.
  window.dispatchEvent(new Event("ligus:theme"));
  syncNativeTitlebar(p);
}

/// The concrete scheme currently applied to the document.
export function currentScheme(): "light" | "dark" {
  return document.documentElement.style.colorScheme === "dark" ? "dark" : "light";
}

/// React to the applied light/dark scheme — for embeds that can't consume the
/// CSS custom properties directly (Monaco themes, xterm themes).
export function useColorScheme(): "light" | "dark" {
  return useSyncExternalStore(
    (cb) => {
      window.addEventListener("ligus:theme", cb);
      return () => window.removeEventListener("ligus:theme", cb);
    },
    currentScheme,
    () => "light" as const,
  );
}

// Tint the native OS title bar to match the canvas (Windows 11; no-op elsewhere
// and outside Tauri). Best-effort — a failure here must never break theming.
function syncNativeTitlebar(p: Palette) {
  const m = p.canvas.match(/(\d+(?:\.\d+)?)/g);
  if (!m || m.length < 3) return;
  const [r, g, b] = m.slice(0, 3).map((n) => Math.round(Number(n)));
  void setTitlebarColor(r, g, b, p.scheme === "dark").catch(() => {});
}
