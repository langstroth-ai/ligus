// Derive a Monaco editor theme from Ligus's `--ligus-*` CSS tokens so the code
// editor's chrome matches whatever palette (incl. Obsidian) is applied to the
// React shell. Like `DiffView` we can't feed Monaco the CSS custom properties
// directly — Monaco wants concrete hex colours — so we read the computed tokens
// off `:root`, convert them to hex, and register/apply a named theme. It is
// re-applied on the `ligus:theme` window event (dispatched by `applyTheme`).
import * as monacoNs from "monaco-editor";
import { currentScheme } from "@/lib/theme";

type Monaco = typeof monacoNs;

/// Name to pass to `<Editor theme=… />` / `monaco.editor.setTheme`.
export const MONACO_THEME = "ligus-tokens";

function readToken(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/// Convert an `rgb()/rgba()`/`#hex` colour string to a Monaco-acceptable hex
/// (`#rrggbb` or `#rrggbbaa`). Returns undefined for anything unparseable so
/// callers can drop the key rather than feed Monaco an invalid colour.
function toHex(color: string): string | undefined {
  if (!color) return undefined;
  if (color.startsWith("#")) return color;
  const m = color.match(/rgba?\(([^)]+)\)/i);
  if (!m) return undefined;
  const parts = m[1].split(/[,\s/]+/).filter(Boolean);
  const [r, g, b] = parts.slice(0, 3).map((n) => Math.round(Number(n)));
  if ([r, g, b].some((n) => Number.isNaN(n))) return undefined;
  const a = parts.length > 3 ? Number(parts[3]) : 1;
  const hh = (n: number) => Math.max(0, Math.min(255, n)).toString(16).padStart(2, "0");
  let out = `#${hh(r)}${hh(g)}${hh(b)}`;
  if (Number.isFinite(a) && a < 1) out += hh(Math.round(a * 255));
  return out;
}

/// Build the `colors` map for `defineTheme`, dropping any token that didn't
/// resolve so Monaco never receives an invalid value.
function buildColors(): Record<string, string> {
  const canvas = toHex(readToken("--ligus-canvas"));
  const panel = toHex(readToken("--ligus-panel"));
  const ink = toHex(readToken("--ligus-ink"));
  const mist = toHex(readToken("--ligus-mist"));
  const line = toHex(readToken("--ligus-line"));
  const accent = toHex(readToken("--ligus-accent-cool")) ?? toHex(readToken("--ligus-accent-warm"));
  const overlay = toHex(readToken("--ligus-overlay"));

  const wanted: Record<string, string | undefined> = {
    "editor.background": canvas,
    "editor.foreground": ink,
    "editorGutter.background": canvas,
    "editorLineNumber.foreground": ink && ink + "66",
    "editorLineNumber.activeForeground": ink,
    "editor.lineHighlightBackground": mist,
    "editor.lineHighlightBorder": "#00000000",
    "editorCursor.foreground": accent,
    "editor.selectionBackground": accent && accent.slice(0, 7) + "40",
    "editor.inactiveSelectionBackground": accent && accent.slice(0, 7) + "22",
    "editorIndentGuide.background1": line,
    "editorIndentGuide.activeBackground1": ink && ink + "44",
    "editorWhitespace.foreground": ink && ink + "33",
    "editorWidget.background": panel,
    "editorWidget.border": line,
    "editorHoverWidget.background": panel,
    "editorHoverWidget.border": line,
    "editorSuggestWidget.background": panel,
    "editorSuggestWidget.border": line,
    "editorSuggestWidget.selectedBackground": overlay,
    "input.background": panel,
    "input.border": line,
    "dropdown.background": panel,
    "dropdown.border": line,
    "minimap.background": canvas,
    "scrollbarSlider.background": overlay,
    "scrollbarSlider.hoverBackground": ink && ink + "22",
    "scrollbarSlider.activeBackground": ink && ink + "33",
    "editorOverviewRuler.border": "#00000000",
    focusBorder: accent,
  };

  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(wanted)) if (v) out[k] = v;
  return out;
}

/// Define + apply the Ligus theme. Inherits syntax token colours from the
/// nearest built-in base (vs / vs-dark) and overrides the editor chrome with
/// Ligus tokens. Safe to call repeatedly (idempotent redefine).
export function applyMonacoTheme(monaco: Monaco = monacoNs): void {
  const dark = currentScheme() === "dark";
  monaco.editor.defineTheme(MONACO_THEME, {
    base: dark ? "vs-dark" : "vs",
    inherit: true,
    rules: [],
    colors: buildColors(),
  });
  monaco.editor.setTheme(MONACO_THEME);
}

/// Re-apply the theme whenever the shell's palette/scheme flips. Returns an
/// unsubscribe.
export function watchMonacoTheme(monaco: Monaco = monacoNs): () => void {
  const onThemeChange = () => applyMonacoTheme(monaco);
  window.addEventListener("ligus:theme", onThemeChange);
  return () => window.removeEventListener("ligus:theme", onThemeChange);
}
