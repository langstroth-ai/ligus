#!/usr/bin/env node
// Design QA against the mock: renders the desktop app (bun run dev:mock) in
// headless Chrome for several themes and views, and checks each capture for
//
//   contrast     text below WCAG AA (4.5:1, or 3:1 for large text)
//   small text   any visible text below 11px
//   banners      more than one banner at once (elements marked data-calm="banner")
//   badges       too many count badges (filled pills holding only a number)
//   accent       too many accent-filled regions (the accent marks an action or a state)
//
// Limits are the calm rules. What already breaks a rule today is recorded in
// tools/design-qa-baseline.json and tolerated, so nothing gets worse: a
// contrast or small-text failure is known by its text (and theme, for
// contrast), so layout differences between machines don't matter; banners,
// badges and accent regions are counts per capture. The baseline only ever
// shrinks: rerun with --update-baseline after a change that improves it.
//
//   bun run dev:mock &                       # serves http://localhost:5199/
//   node tools/scan.mjs [--out DIR] [--only chat,review] [--update-baseline]
//
// Options: --base URL (default http://localhost:5199/), --out DIR (screenshots
// and report.json), --chrome PATH (or CHROME_PATH; default: the installed
// Google Chrome or Chromium). Exit 0 when every capture is within its limits,
// 1 when one is over, 2 on a usage error.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const here = path.dirname(fileURLToPath(import.meta.url));
const BASELINE = path.join(here, "design-qa-baseline.json");

export const LIMITS = { contrast: 0, smallText: 0, banners: 1, badges: 3, accent: 4 };

export const THEMES = [
  ["pollen", "light"],
  ["pollen", "dark"],
  ["studio", "light"],
  ["harbor", "dark"],
  ["slate", "light"],
  ["meadow", "dark"],
  ["obsidian", "dark"],
];

const pane = (name) => async (p) => {
  await p.getByRole("button", { name: new RegExp(`^${name}( \\d+)?$`) }).first().click();
};
const settings = (tab) => async (p) => {
  await p.keyboard.press("Meta+,");
  await p.waitForTimeout(300);
  await p.getByRole("dialog").getByRole("button", { name: tab, exact: true }).first().click();
};

export const VIEWS = [
  ["chat", "", null],
  ["chat-guards", "dearth=1&lockdown=1", null],
  ["review", "", pane("Review")],
  ["workflows", "", pane("Workflows")],
  ["people", "", pane("People")],
  ["settings", "", settings("Appearance")],
];

function args(argv) {
  const out = { base: "http://localhost:5199/", out: null, only: null, update: false, chrome: process.env.CHROME_PATH ?? null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const [k, inline] = a.includes("=") ? [a.slice(0, a.indexOf("=")), a.slice(a.indexOf("=") + 1)] : [a, null];
    const val = () => inline ?? argv[++i];
    if (k === "--base") out.base = val();
    else if (k === "--out") out.out = val();
    else if (k === "--only") out.only = new Set(val().split(","));
    else if (k === "--chrome") out.chrome = val();
    else if (k === "--update-baseline") out.update = true;
    else if (k === "-h" || k === "--help") {
      console.log(readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").slice(1, 26).map((l) => l.replace(/^\/\/ ?/, "")).join("\n"));
      process.exit(0);
    } else {
      console.error(`scan: unknown option ${a}`);
      process.exit(2);
    }
  }
  return out;
}

function findChrome(explicit) {
  const candidates = [
    explicit,
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ].filter(Boolean);
  return candidates.find((c) => existsSync(c)) ?? null;
}

/// Runs in the page: measures one capture.
function measure() {
  const cv = document.createElement("canvas");
  cv.width = cv.height = 1;
  const g = cv.getContext("2d", { willReadFrequently: true });
  const rgba = (c) => {
    g.clearRect(0, 0, 1, 1);
    g.fillStyle = "#000";
    g.fillStyle = c;
    g.fillRect(0, 0, 1, 1);
    const d = g.getImageData(0, 0, 1, 1).data;
    return [d[0], d[1], d[2], d[3] / 255];
  };
  const over = (f, b) => [0, 1, 2].map((i) => f[i] * f[3] + b[i] * (1 - f[3]));
  const lin = (v) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
  const lum = (c) => 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
  const ratio = (a, b) => {
    const [x, y] = [lum(a), lum(b)];
    return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
  };
  const near = (a, b) => Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) < 18;
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0 || r.bottom < 0 || r.top > innerHeight || r.right < 0 || r.left > innerWidth) return false;
    const cs = getComputedStyle(el);
    if (cs.visibility === "hidden" || cs.display === "none") return false;
    let op = 1;
    for (let e = el; e; e = e.parentElement) op *= Number(getComputedStyle(e).opacity);
    return op >= 0.05;
  };
  const bgOf = (el) => {
    const stack = [];
    for (let e = el; e; e = e.parentElement) {
      const cs = getComputedStyle(e);
      if (cs.backgroundImage && cs.backgroundImage !== "none" && !cs.backgroundImage.startsWith("url")) return null;
      const c = rgba(cs.backgroundColor);
      if (c[3] > 0) {
        stack.push(c);
        if (c[3] >= 0.99) break;
      }
    }
    let bg = [255, 255, 255];
    for (let i = stack.length - 1; i >= 0; i--) bg = over(stack[i], bg);
    return bg;
  };

  // Accent colours as this theme paints them.
  const probe = document.createElement("div");
  document.body.appendChild(probe);
  const accents = ["--ligus-accent-cool", "--ligus-accent-fill", "--ligus-warm-fill", "--ligus-accent-warm"].map((v) => {
    probe.style.background = `var(${v})`;
    return rgba(getComputedStyle(probe).backgroundColor);
  });
  probe.remove();

  const root = document.querySelector('[role="dialog"]') ?? document.body;
  const contrast = [];
  const smallText = [];
  const seen = new Set();
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let n;
  while ((n = walker.nextNode())) {
    const t = n.textContent.trim();
    if (!t) continue;
    const el = n.parentElement;
    if (!el || seen.has(el)) continue;
    seen.add(el);
    if (el.closest('.monaco-editor, .xterm, textarea, input, select, option, .sr-only, [aria-hidden="true"]')) continue;
    if (!visible(el)) continue;
    const cs = getComputedStyle(el);
    const size = parseFloat(cs.fontSize);
    if (size < 10.95) smallText.push({ text: t.slice(0, 40), size });
    if (el.closest("button:disabled, [aria-disabled=true], code.hljs, .hljs")) continue;
    const bg = bgOf(el);
    if (!bg) continue;
    let op = 1;
    for (let e = el; e; e = e.parentElement) op *= Number(getComputedStyle(e).opacity);
    const fg = rgba(cs.color);
    fg[3] *= op;
    const cr = ratio(over(fg, bg), bg);
    const bold = Number(cs.fontWeight) >= 700;
    const need = size >= 24 || (bold && size >= 18.66) ? 3 : 4.5;
    if (cr < need) contrast.push({ text: t.slice(0, 40), ratio: Math.round(cr * 100) / 100, size });
  }

  const banners = [...document.querySelectorAll('[data-calm="banner"]')].filter(visible).length;

  // A count badge: a filled, rounded pill whose whole text is a number.
  let badges = 0;
  const accentEls = [];
  for (const el of document.querySelectorAll("body *")) {
    if (!(el instanceof HTMLElement) || !visible(el)) continue;
    const cs = getComputedStyle(el);
    const bg = rgba(cs.backgroundColor);
    if (bg[3] < 0.5) continue;
    const r = el.getBoundingClientRect();
    if (/^\d+\+?$/.test(el.textContent.trim()) && parseFloat(cs.borderTopLeftRadius) >= 6 && r.width <= 40) badges++;
    if (r.width * r.height >= 64 && accents.some((a) => a[3] > 0 && near(bg, a))) accentEls.push(el);
  }
  // Nested accent fills count once.
  const accent = accentEls.filter((el) => !accentEls.some((o) => o !== el && o.contains(el))).length;

  return { contrast, smallText, banners, badges, accent };
}

async function main() {
  const opt = args(process.argv.slice(2));
  const chrome = findChrome(opt.chrome);
  if (!chrome) {
    console.error("scan: no Chrome or Chromium found; pass --chrome PATH or set CHROME_PATH");
    process.exit(2);
  }
  if (opt.out) mkdirSync(opt.out, { recursive: true });
  const baseline = existsSync(BASELINE) ? JSON.parse(readFileSync(BASELINE, "utf8")) : {};
  const browser = await chromium.launch({ executablePath: chrome, headless: true });
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce" });
  const results = [];
  for (const [theme, mode] of THEMES) {
    for (const [view, query, act] of VIEWS) {
      if (opt.only && !opt.only.has(view)) continue;
      const id = `${view}--${theme}-${mode}`;
      const page = await ctx.newPage();
      page.setDefaultTimeout(5000);
      await page.emulateMedia({ colorScheme: mode, reducedMotion: "reduce" });
      await page.addInitScript(([t, m]) => {
        localStorage.clear();
        localStorage.setItem("ligus.theme", t);
        localStorage.setItem("ligus.appearance", m);
      }, [theme, mode]);
      await page.goto(opt.base + (query ? `?${query}` : ""));
      await page.waitForTimeout(900);
      let error = null;
      if (act) {
        try {
          await act(page);
        } catch (e) {
          error = String(e.message).split("\n")[0];
        }
        await page.waitForTimeout(600);
      }
      const m = await page.evaluate(measure);
      if (opt.out) await page.screenshot({ path: path.join(opt.out, `${id}.png`) });
      await page.close();
      const counts = { contrast: m.contrast.length, smallText: m.smallText.length, banners: m.banners, badges: m.badges, accent: m.accent };
      const sigs = {
        contrast: [...new Set(m.contrast.map((c) => `${theme}-${mode}|${c.text}`))],
        smallText: [...new Set(m.smallText.map((c) => `${c.text}@${c.size}`))],
      };
      const known = { contrast: new Set(baseline.known?.contrast ?? []), smallText: new Set(baseline.known?.smallText ?? []) };
      const fresh = { contrast: sigs.contrast.filter((x) => !known.contrast.has(x)), smallText: sigs.smallText.filter((x) => !known.smallText.has(x)) };
      const over = [];
      if (fresh.contrast.length > LIMITS.contrast) over.push("contrast");
      if (fresh.smallText.length > LIMITS.smallText) over.push("smallText");
      for (const k of ["banners", "badges", "accent"]) if (counts[k] > Math.max(LIMITS[k], baseline.counts?.[id]?.[k] ?? 0)) over.push(k);
      results.push({ id, counts, sigs, over, error, detail: { contrast: fresh.contrast.slice(0, 8), smallText: fresh.smallText.slice(0, 8) } });
      const flag = error ? "ERROR" : over.length ? "OVER " : "ok   ";
      console.error(`${flag} ${id.padEnd(28)} ${Object.entries(counts).map(([k, v]) => `${k} ${v}`).join("  ")}${over.length ? `  (over: ${over.join(", ")})` : ""}${error ? `  (${error})` : ""}`);
    }
  }
  await browser.close();

  if (opt.out) writeFileSync(path.join(opt.out, "report.json"), JSON.stringify({ limits: LIMITS, results }, null, 2) + "\n");
  if (opt.update) {
    // Keep only what still breaks a rule, and never add to what was known.
    const had = (k, x) => !existsSync(BASELINE) || (baseline.known?.[k] ?? []).includes(x);
    const next = { known: { contrast: [], smallText: [] }, counts: {} };
    for (const k of ["contrast", "smallText"]) {
      next.known[k] = [...new Set(results.flatMap((r) => r.sigs[k]))].filter((x) => had(k, x)).sort();
    }
    for (const r of results) {
      const keep = {};
      for (const k of ["banners", "badges", "accent"]) {
        const v = r.counts[k];
        if (v > LIMITS[k]) keep[k] = existsSync(BASELINE) ? Math.min(v, baseline.counts?.[r.id]?.[k] ?? LIMITS[k]) : v;
      }
      if (Object.keys(keep).length) next.counts[r.id] = keep;
    }
    writeFileSync(BASELINE, JSON.stringify(next, null, 2) + "\n");
    console.error(`baseline written: ${next.known.contrast.length} contrast and ${next.known.smallText.length} small-text texts, ${Object.keys(next.counts).length} captures over a count`);
  }
  const failed = results.filter((r) => r.over.length || r.error);
  for (const r of failed) {
    for (const k of r.over) {
      const items = r.detail[k];
      const limit = items ? "new" : `> ${Math.max(LIMITS[k], baseline.counts?.[r.id]?.[k] ?? 0)}`;
      console.error(`  ${r.id}: ${k} ${limit}${items ? ": " + items.slice(0, 4).map((x) => JSON.stringify(x)).join(" ") : ` (${r.counts[k]})`}`);
    }
  }
  console.error(`${results.length - failed.length}/${results.length} captures within the calm and contrast limits`);
  process.exit(failed.length && !opt.update ? 1 : 0);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) await main();
