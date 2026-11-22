#!/usr/bin/env node
// Checks the token source (../ligus-brand-tokens.json) for legibility and
// colour-vision safety. Exits 1 if any check fails; prints one line per check
// group, and every failure. `--json` prints the full report on stdout.
//
//   node assets/branding/tokens/validate.mjs [--json]
//
// What it checks, for every palette in light and dark:
//   - ink and the status inks as text: 4.5:1 on canvas, panel and mist;
//   - status colours as glyphs (non-text): 3:1 on canvas, panel and mist;
//   - text on a filled accent control: 4.5:1;
//   - diff inks on their own line tint: 4.5:1; the comment marker 3:1;
//   - presence colours: 3:1 against panel;
//   - warn against accent-warm: far enough apart (they mean different things);
//   - every authored colour sits inside sRGB.
// And for each scheme, every pair of status colours stays distinguishable
// with normal vision and simulated protanopia, deuteranopia and tritanopia.
// Status never relies on colour alone (each state has a glyph); this keeps
// the colours from collapsing into one another when the glyph is small.

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CVD_KINDS, atLightness, contrast, deltaE, hueGap, inGamut, over, parse, parseOklch, rgbOf, rgbToOklch, simulate } from "./color.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
export const TOKENS_PATH = path.join(here, "..", "ligus-brand-tokens.json");

/** Minimum OKLab distance between two status colours, under every simulation. */
export const STATUS_MIN_DELTA_E = 0.05;
/** Minimum OKLab distance between warn and a palette's accent-warm. */
export const WARN_WARM_MIN_DELTA_E = 0.06;

const pct = (s) => Number(String(s).replace("%", "")) / 100;
const rgb = (css) => rgbOf(parse(css));

/** Run every check. Returns { ok, failures: string[], checks: {id, pass, detail}[] }. */
export function validate(tokens) {
  const checks = [];
  const add = (id, pass, detail) => checks.push({ id, pass, detail });

  for (const [name, family] of Object.entries(tokens.themes)) {
    for (const [variant, p] of Object.entries(family)) {
      const s = tokens.schemes[p.scheme];
      const where = `${name}/${variant}`;
      const surfaces = { canvas: rgb(p.canvas), panel: rgb(p.panel), mist: rgb(p.mist) };
      const inkL = pct(s.numbers.accentInkL);
      const fillL = pct(s.numbers.fillL);
      for (const [sname, surface] of Object.entries(surfaces)) {
        const ink = contrast(rgb(p.ink), surface);
        add(`${where}: ink on ${sname}`, ink >= 4.5, ink.toFixed(2));
        for (const [role, css] of Object.entries(s.status)) {
          const glyph = contrast(rgb(css), surface);
          add(`${where}: ${role} glyph on ${sname}`, glyph >= 3, glyph.toFixed(2));
          const text = contrast(atLightness(rgb(css), inkL), surface);
          add(`${where}: ${role} ink on ${sname}`, text >= 4.5, text.toFixed(2));
        }
        const warnText = contrast(rgb(s.status.warn), surface);
        add(`${where}: warn as text on ${sname}`, warnText >= 4.5, warnText.toFixed(2));
        const warmText = contrast(atLightness(rgb(p.accentWarm), inkL), surface);
        add(`${where}: accent-warm ink on ${sname}`, warmText >= 4.5, warmText.toFixed(2));
      }
      const fill = contrast(rgb(s.onAccent), atLightness(rgb(p.accentCool), fillL));
      add(`${where}: on-accent text on accent fill`, fill >= 4.5, fill.toFixed(2));

      const panel = parse(p.panel);
      for (const kind of ["add", "del"]) {
        const bg = over(parse(s.diff[`${kind}Bg`]), panel);
        const c = contrast(rgb(s.diff[`${kind}Ink`]), bg);
        add(`${where}: diff ${kind} ink on its tint`, c >= 4.5, c.toFixed(2));
      }
      const comment = contrast(rgb(s.diff.comment), surfaces.panel);
      add(`${where}: diff comment marker on panel`, comment >= 3, comment.toFixed(2));
      s.presence.forEach((css, i) => {
        const c = contrast(rgb(css), surfaces.panel);
        add(`${where}: presence-${i + 1} on panel`, c >= 3, c.toFixed(2));
      });

      const warn = rgb(s.status.warn);
      const warm = rgb(p.accentWarm);
      const gap = hueGap(rgbToOklch(warn)[2], rgbToOklch(warm)[2]);
      for (const vision of ["normal", ...CVD_KINDS]) {
        const sim = (c) => (vision === "normal" ? c : simulate(c, vision));
        const d = deltaE(sim(warn), sim(warm));
        add(`${where}: warn vs accent-warm (${vision})`, d >= WARN_WARM_MIN_DELTA_E, `ΔE ${d.toFixed(3)}, hue gap ${gap.toFixed(1)}°`);
      }
    }
  }

  const gamut = (where, css) => add(`${where}: inside sRGB`, inGamut(parseOklch(css).lch), css);
  for (const [name, family] of Object.entries(tokens.themes)) {
    for (const [variant, p] of Object.entries(family)) {
      for (const [role, css] of Object.entries(p)) if (role !== "scheme") gamut(`${name}/${variant} ${role}`, css);
    }
  }
  for (const [scheme, s] of Object.entries(tokens.schemes)) {
    for (const [role, css] of Object.entries(s.status)) gamut(`${scheme} ${role}`, css);
    for (const [role, css] of Object.entries(s.diff)) gamut(`${scheme} diff ${role}`, css);
    s.presence.forEach((css, i) => gamut(`${scheme} presence-${i + 1}`, css));
  }

  for (const [scheme, s] of Object.entries(tokens.schemes)) {
    const roles = Object.entries(s.status);
    for (const vision of ["normal", ...CVD_KINDS]) {
      const sim = (c) => (vision === "normal" ? c : simulate(c, vision));
      for (let i = 0; i < roles.length; i++) {
        for (let j = i + 1; j < roles.length; j++) {
          const d = deltaE(sim(rgb(roles[i][1])), sim(rgb(roles[j][1])));
          add(`${scheme}: ${roles[i][0]} vs ${roles[j][0]} (${vision})`, d >= STATUS_MIN_DELTA_E, `ΔE ${d.toFixed(3)}`);
        }
      }
    }
  }

  for (const [state, meta] of Object.entries(tokens.states)) {
    add(`state ${state}: has a glyph on every surface`, Boolean(meta.icon && meta.sfSymbol && meta.lt && meta.ascii && meta.word), "");
    const known = meta.role === "accentWarm" || meta.role in tokens.schemes.light.status;
    add(`state ${state}: role is a status role or accent-warm`, known, meta.role);
  }

  const failures = checks.filter((c) => !c.pass).map((c) => `${c.id}: ${c.detail}`);
  return { ok: failures.length === 0, failures, checks };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  const tokens = JSON.parse(readFileSync(TOKENS_PATH, "utf8"));
  const report = validate(tokens);
  if (process.argv.includes("--json")) {
    process.stdout.write(JSON.stringify(report, null, 2) + "\n");
  } else {
    for (const f of report.failures) console.error(`FAIL ${f}`);
    console.error(`${report.checks.length - report.failures.length}/${report.checks.length} token checks pass`);
  }
  process.exit(report.ok ? 0 : 1);
}
