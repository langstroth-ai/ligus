import { describe, expect, it } from "vitest";
import { STATUS_COLORS, THEMES, type Palette } from "./theme";

// WCAG 2.x contrast + OKLCH helpers, enough to check the token pairs the CSS
// derives with `oklch(from …)` (styles.css). Out-of-gamut results are clipped
// per channel, as Chromium/WebKit render them.
type RGB = [number, number, number];
const parse = (c: string): RGB => {
  const m = c.match(/[\d.]+/g);
  if (!m || m.length < 3) throw new Error(`not an rgb() colour: ${c}`);
  return [Number(m[0]), Number(m[1]), Number(m[2])];
};
const lin = (v: number) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const delin = (x: number) => {
  const c = Math.min(1, Math.max(0, x));
  return 255 * (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);
};
const luminance = ([r, g, b]: RGB) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
const contrast = (a: RGB, b: RGB) => {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};
const toOklch = (rgb: RGB): [number, number, number] => {
  const [r, g, b] = rgb.map(lin);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const B = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  let h = (Math.atan2(B, A) * 180) / Math.PI;
  if (h < 0) h += 360;
  return [L, Math.hypot(A, B), h];
};
const fromOklch = ([L, C, H]: [number, number, number]): RGB => {
  const a = C * Math.cos((H * Math.PI) / 180);
  const b = C * Math.sin((H * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    delin(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    delin(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    delin(-0.0041960863 * l - 0.7034186147 * m + 1.707612701 * s),
  ];
};
/// `oklch(from <c> <L> c h)`: the colour at a fixed OKLCH lightness.
const atLightness = (c: string, L: number): RGB => {
  const [, C, H] = toOklch(parse(c));
  return fromOklch([L, C, H]);
};
const hueGap = (a: number, b: number) => {
  const d = Math.abs(a - b) % 360;
  return d > 180 ? 360 - d : d;
};

// Scheme numbers from applyTheme(): --ligus-accent-ink-l, --ligus-fill-l, --ligus-on-accent.
const SCHEME = {
  light: { inkL: 0.42, fillL: 0.45, onAccent: "rgb(255,255,255)" },
  dark: { inkL: 0.84, fillL: 0.82, onAccent: "rgb(18,20,24)" },
} as const;

const variants: [string, Palette][] = Object.entries(THEMES).flatMap(([name, v]) => [
  [`${name}/light`, v.light] as [string, Palette],
  [`${name}/dark`, v.dark] as [string, Palette],
]);

describe("warn status colour", () => {
  it.each(variants)("%s: warn and warn-ink stay WCAG AA as text on every surface", (_, p) => {
    const warn = STATUS_COLORS[p.scheme].warn;
    const warnInk = atLightness(warn, SCHEME[p.scheme].inkL);
    for (const surface of [p.canvas, p.panel, p.mist]) {
      expect(contrast(parse(warn), parse(surface))).toBeGreaterThanOrEqual(4.5);
      expect(contrast(warnInk, parse(surface))).toBeGreaterThanOrEqual(4.5);
    }
  });

  it.each(["light", "dark"] as const)("%s: warn's hue sits clearly apart from the honey brand accent", (scheme) => {
    const [, , warnHue] = toOklch(parse(STATUS_COLORS[scheme].warn));
    const [, , honeyHue] = toOklch(parse(THEMES.pollen[scheme].accentCool));
    const [, , dangerHue] = toOklch(parse(STATUS_COLORS[scheme].danger));
    expect(hueGap(warnHue, honeyHue)).toBeGreaterThanOrEqual(25);
    expect(hueGap(warnHue, dangerHue)).toBeGreaterThanOrEqual(15);
  });
});

describe("filled accent controls", () => {
  // The pair components use for a filled control with text on it:
  // background --ligus-accent-fill, colour --ligus-on-accent (never the raw
  // --ligus-accent-cool, which is about 2.5:1 with white text in pollen).
  it.each(variants)("%s: on-accent text on accent-fill is at least 4.5:1", (_, p) => {
    const s = SCHEME[p.scheme];
    expect(contrast(parse(s.onAccent), atLightness(p.accentCool, s.fillL))).toBeGreaterThanOrEqual(4.5);
  });
});

// Static guard against unreadable filled controls: text painted on the
// raw honey fill, or white/black literals instead of the on-accent token.
const sources = import.meta.glob("../components/**/*.tsx", { query: "?raw", import: "default", eager: true }) as Record<string, string>;

describe("component colour lint", () => {
  const files = Object.entries(sources).filter(([path]) => !path.includes(".test."));

  it("finds the component sources", () => {
    expect(files.length).toBeGreaterThan(50);
  });

  it("never pairs a raw accent-cool background with a text colour", () => {
    const offenders: string[] = [];
    for (const [path, src] of files) {
      // Inline style objects: `{ … background: …accent-cool… color: … }`.
      for (const m of src.matchAll(/style=\{\{([^{}]*)\}\}/g)) {
        const body = m[1];
        if (/background:[^,]*var\(--ligus-accent-cool\)/.test(body) && /(^|[\s,])color:/.test(body)) {
          offenders.push(`${path}: ${body.trim().slice(0, 120)}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it("uses tokens, not #fff/#000 literals, for text colour", () => {
    const offenders: string[] = [];
    for (const [path, src] of files) {
      // Avatar paints initials on generated per-person colours; it owns that pairing.
      if (path.endsWith("/Avatar.tsx")) continue;
      for (const m of src.matchAll(/\bcolor:\s*[^,}\n]*["'](#fff|#ffffff|#000|#000000|white|black)["']/gi)) {
        offenders.push(`${path}: ${m[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
