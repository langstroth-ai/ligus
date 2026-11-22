// Colour maths shared by the token generator, the validator and the web tests.
// No dependencies: OKLCH <-> sRGB (Björn Ottosson's OKLab), WCAG 2.x contrast,
// alpha compositing, colour-vision-deficiency simulation (Machado, Oliveira and
// Fernandes 2009, full severity, applied in linear sRGB) and the nearest
// xterm-256 index for a colour.

/** @typedef {[number, number, number]} Triple */
/** A parsed colour: sRGB channels 0..255 (floats, unclipped until `clip`) and alpha 0..1. */
/** @typedef {{ r: number, g: number, b: number, a: number }} Rgba */

const lin = (v) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const delin = (x) => 255 * (x <= 0.0031308 ? 12.92 * x : 1.055 * Math.sign(x) * Math.abs(x) ** (1 / 2.4) - 0.055);

/** OKLab of an sRGB colour (0..255 channels). */
export function rgbToOklab([r, g, b]) {
  const [R, G, B] = [r, g, b].map(lin);
  const l = Math.cbrt(0.4122214708 * R + 0.5363325363 * G + 0.0514459929 * B);
  const m = Math.cbrt(0.2119034982 * R + 0.6806995451 * G + 0.1073969566 * B);
  const s = Math.cbrt(0.0883024619 * R + 0.2817188376 * G + 0.6299787005 * B);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** OKLCH (L 0..1, C, H degrees) of an sRGB colour. */
export function rgbToOklch(rgb) {
  const [L, A, B] = rgbToOklab(rgb);
  let h = (Math.atan2(B, A) * 180) / Math.PI;
  if (h < 0) h += 360;
  return [L, Math.hypot(A, B), h];
}

/** sRGB (0..255, unclipped) of an OKLCH colour. */
export function oklchToRgb([L, C, H]) {
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
}

/** True when an OKLCH colour fits sRGB (within half a channel step). */
export function inGamut(lch) {
  return oklchToRgb(lch).every((v) => v >= -0.5 && v <= 255.5);
}

/** Clip to 0..255 and round, the way browsers paint an out-of-gamut colour. */
export const clip = (rgb) => rgb.map((v) => Math.round(Math.min(255, Math.max(0, v))));

/**
 * Parse `oklch(L C H)` / `oklch(L C H / A)` (L as 0..1 or a percentage),
 * `rgb(r,g,b)`, `rgba(r,g,b,a)` or `#rrggbb`.
 * @returns {Rgba}
 */
export function parse(css) {
  const s = css.trim();
  let m = s.match(/^oklch\(\s*([\d.]+)(%?)\s+([\d.]+)\s+([\d.]+)\s*(?:\/\s*([\d.]+)\s*)?\)$/);
  if (m) {
    const L = Number(m[1]) / (m[2] ? 100 : 1);
    const [r, g, b] = oklchToRgb([L, Number(m[3]), Number(m[4])]);
    return { r, g, b, a: m[5] === undefined ? 1 : Number(m[5]) };
  }
  m = s.match(/^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/);
  if (m) return { r: Number(m[1]), g: Number(m[2]), b: Number(m[3]), a: m[4] === undefined ? 1 : Number(m[4]) };
  m = s.match(/^#([0-9a-f]{6})$/i);
  if (m) {
    const n = parseInt(m[1], 16);
    return { r: n >> 16, g: (n >> 8) & 255, b: n & 255, a: 1 };
  }
  throw new Error(`unsupported colour: ${css}`);
}

/** Parse an `oklch(...)` string into its numbers. */
export function parseOklch(css) {
  const m = css.trim().match(/^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*(?:\/\s*([\d.]+)\s*)?\)$/);
  if (!m) throw new Error(`not an oklch() colour: ${css}`);
  return { lch: [Number(m[1]), Number(m[2]), Number(m[3])], a: m[4] === undefined ? 1 : Number(m[4]) };
}

/** The clipped, rounded channels of a parsed colour. */
export const rgbOf = (c) => clip([c.r, c.g, c.b]);

/** `rgb(r,g,b)` or `rgba(r,g,b,a)`: the fallback form the desktop has always used. */
export function toRgbCss(css) {
  const c = parse(css);
  const [r, g, b] = rgbOf(c);
  return c.a >= 1 ? `rgb(${r},${g},${b})` : `rgba(${r},${g},${b},${Number(c.a.toFixed(3))})`;
}

/** `#rrggbb` (alpha dropped). */
export function toHex(css) {
  return "#" + rgbOf(parse(css)).map((v) => v.toString(16).padStart(2, "0")).join("");
}

/** Paint `fg` (with its alpha) over an opaque `bg`; both Rgba. Returns rounded RGB. */
export function over(fg, bg) {
  const f = rgbOf(fg);
  const b = rgbOf(bg);
  return f.map((v, i) => v * fg.a + b[i] * (1 - fg.a));
}

const luminance = ([r, g, b]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);

/** WCAG 2.x contrast ratio of two opaque RGB triples. */
export function contrast(a, b) {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** `oklch(from c L c h)`: the same chroma and hue at a fixed OKLCH lightness. */
export function atLightness(rgb, L) {
  const [, C, H] = rgbToOklch(rgb);
  return clip(oklchToRgb([L, C, H]));
}

/** Shortest distance between two hues, in degrees. */
export function hueGap(a, b) {
  const d = Math.abs(a - b) % 360;
  return d > 180 ? 360 - d : d;
}

/** Euclidean distance in OKLab (about 0.02 is a just-noticeable difference). */
export function deltaE(a, b) {
  const [x, y] = [rgbToOklab(a), rgbToOklab(b)];
  return Math.hypot(x[0] - y[0], x[1] - y[1], x[2] - y[2]);
}

// Machado et al. 2009, severity 1.0, for linear sRGB.
const CVD = {
  protanopia: [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
  ],
  deuteranopia: [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.01182, 0.04294, 0.968881],
  ],
  tritanopia: [
    [1.255528, -0.076749, -0.178779],
    [-0.078411, 0.930809, 0.147602],
    [0.004733, 0.691367, 0.3039],
  ],
};
export const CVD_KINDS = /** @type {const} */ (["protanopia", "deuteranopia", "tritanopia"]);

/** How an opaque RGB colour looks with a colour-vision deficiency. */
export function simulate(rgb, kind) {
  const M = CVD[kind];
  const v = rgb.map(lin);
  const out = M.map((row) => row[0] * v[0] + row[1] * v[1] + row[2] * v[2]);
  return clip(out.map((x) => delin(Math.min(1, Math.max(0, x)))));
}

// xterm-256: indexes 16..231 are a 6x6x6 cube, 232..255 a grey ramp. 0..15 are
// the user's own palette, so a fixed colour never maps to them.
const CUBE = [0, 95, 135, 175, 215, 255];
export const XTERM = (() => {
  const out = [];
  for (let i = 16; i < 232; i++) {
    const n = i - 16;
    out.push([i, [CUBE[Math.floor(n / 36)], CUBE[Math.floor(n / 6) % 6], CUBE[n % 6]]]);
  }
  for (let i = 232; i < 256; i++) {
    const v = 8 + 10 * (i - 232);
    out.push([i, [v, v, v]]);
  }
  return out;
})();

/** The nearest xterm-256 index (16..255) to an RGB colour, by OKLab distance. */
export function nearestXterm(rgb) {
  let best = 16;
  let bestD = Infinity;
  for (const [i, c] of XTERM) {
    const d = deltaE(rgb, c);
    if (d < bestD) {
      bestD = d;
      best = i;
    }
  }
  return best;
}

/** Round an RGB colour to OKLCH numbers that reproduce it exactly when rounded back. */
export function exactOklch(rgb) {
  const [L, C, H] = rgbToOklch(rgb);
  for (const [dl, dc, dh] of [
    [4, 4, 2],
    [5, 5, 3],
    [6, 6, 4],
  ]) {
    const lch = [Number(L.toFixed(dl)), Number(C.toFixed(dc)), Number(H.toFixed(dh))];
    const back = clip(oklchToRgb(lch));
    if (back.every((v, i) => v === Math.round(rgb[i]))) return lch;
  }
  return [L, C, H];
}

/** Format OKLCH numbers as CSS. */
export function oklchCss([L, C, H], a = 1) {
  const body = `${L} ${C} ${H}`;
  return a >= 1 ? `oklch(${body})` : `oklch(${body} / ${a})`;
}
