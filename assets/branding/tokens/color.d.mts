// Types for color.mjs, for the web tests.
export type Triple = [number, number, number];
export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}
export const CVD_KINDS: readonly ["protanopia", "deuteranopia", "tritanopia"];
export function parse(css: string): Rgba;
export function rgbOf(c: Rgba): Triple;
export function toRgbCss(css: string): string;
export function contrast(a: Triple, b: Triple): number;
export function deltaE(a: Triple, b: Triple): number;
export function simulate(rgb: Triple, kind: "protanopia" | "deuteranopia" | "tritanopia"): Triple;
export function nearestXterm(rgb: Triple): number;
export function rgbToOklch(rgb: Triple): Triple;
export function oklchToRgb(lch: Triple): Triple;
export function exactOklch(rgb: Triple): Triple;
