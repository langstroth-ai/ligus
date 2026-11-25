import { describe, expect, it } from "vitest";
import tokens from "../../../assets/branding/ligus-brand-tokens.json";
import { contrast, nearestXterm, parse, rgbOf, simulate, toRgbCss } from "../../../assets/branding/tokens/color.mjs";
import { LT_TERMINAL_BACKGROUNDS, ltScheme, readRepoFile, render, stale } from "../../../assets/branding/tokens/generate.mjs";
import { validate } from "../../../assets/branding/tokens/validate.mjs";
import { STATUS_COLORS, THEMES } from "./theme";
import { SCHEME_TOKENS, STATE_GLYPHS } from "./tokens.generated";

// One colour source for the desktop, the phone and lt: these tests fail when
// a generated file drifts from the JSON, or when a surface's values differ.

type Json = typeof tokens;
const clone = (): Json => JSON.parse(JSON.stringify(tokens));

describe("token parity", () => {
  it("every generated file matches the JSON (rerun tokens/generate.mjs if not)", () => {
    expect(stale(tokens)).toEqual([]);
  });

  it("the desktop palettes are the JSON's OKLCH colours, rounded to sRGB", () => {
    for (const [name, family] of Object.entries(tokens.themes)) {
      for (const variant of ["light", "dark"] as const) {
        const p = family[variant];
        const desktop = THEMES[name as keyof typeof THEMES][variant];
        for (const [role, css] of Object.entries(p)) {
          if (role === "scheme") expect(desktop.scheme).toBe(css);
          else expect(desktop[role as keyof typeof desktop]).toBe(toRgbCss(css));
        }
      }
    }
  });

  it("the phone's colour catalogue paints the same channels as the desktop", () => {
    const channels = (rel: string, dark: boolean) => {
      const set = JSON.parse(readRepoFile(`assets/branding/generated/ios/LigusTokens.xcassets/${rel}/Contents.json`));
      const entry = set.colors.find((c: { appearances?: unknown[] }) => Boolean(c.appearances) === dark);
      const { red, green, blue } = entry.color.components;
      return [red, green, blue].map((h: string) => parseInt(h, 16));
    };
    for (const [name, family] of Object.entries(THEMES)) {
      for (const role of ["canvas", "ink", "panel", "accentCool", "accentWarm"] as const) {
        expect(channels(`${name}/${role}.colorset`, false)).toEqual(rgbOf(parse(family.light[role])));
        expect(channels(`${name}/${role}.colorset`, true)).toEqual(rgbOf(parse(family.dark[role])));
      }
    }
    for (const scheme of ["light", "dark"] as const) {
      expect(channels("status/warn.colorset", scheme === "dark")).toEqual(rgbOf(parse(STATUS_COLORS[scheme].warn)));
      expect(channels("status/info.colorset", scheme === "dark")).toEqual(rgbOf(parse(SCHEME_TOKENS[scheme].info)));
    }
  });

  it("lt's table carries the same status inks and a 256-colour index for each", () => {
    const rs = render(tokens)["assets/branding/generated/lt/ligus_tokens.rs"];
    const pollen = rs.slice(rs.indexOf('name: "pollen"'), rs.indexOf('name: "studio"'));
    const dark = pollen.slice(pollen.indexOf("dark: SchemeTokens"));
    const warn = dark.match(/warn: TokenColor \{ rgb: \((\d+), (\d+), (\d+)\), xterm256: (\d+), ansi16: Some\(3\) \}/);
    expect(warn).not.toBeNull();
    const rgb = warn!.slice(1, 4).map(Number) as [number, number, number];
    expect(Number(warn![4])).toBe(nearestXterm(rgb));
    // The accent has no 16-colour hue: yellow belongs to warn.
    expect(dark).toMatch(/accent: TokenColor \{[^}]*ansi16: None \}/);
    // Every palette, for both backgrounds.
    for (const name of Object.keys(tokens.themes)) expect(rs).toContain(`name: "${name}"`);
  });

  it("lt's colours read on its own canvas and on common terminal backgrounds", () => {
    const text = ["text", "dim", "accent", "mention", "ok", "info", "warn", "danger", "neutral", "diff_add", "diff_del", "agent"] as const;
    for (const name of Object.keys(tokens.themes)) {
      for (const variant of ["light", "dark"] as const) {
        const k = ltScheme(tokens, name, variant);
        const backgrounds = [k.canvas, ...Object.values(LT_TERMINAL_BACKGROUNDS[variant])];
        for (const bg of backgrounds) {
          for (const role of text) expect(contrast(k[role], bg), `${name}/${variant} ${role}`).toBeGreaterThanOrEqual(4.5);
          expect(contrast(k.border, bg), `${name}/${variant} border`).toBeGreaterThanOrEqual(3);
          expect(contrast(k.diff_comment, bg), `${name}/${variant} diff comment`).toBeGreaterThanOrEqual(3);
        }
        expect(contrast(k.text, k.selection), `${name}/${variant} text on selection`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("every state has a glyph on every surface, and the desktop shares the table", () => {
    expect(Object.keys(STATE_GLYPHS)).toEqual(Object.keys(tokens.states));
    for (const s of Object.values(STATE_GLYPHS)) {
      expect(s.icon && s.sfSymbol && s.lt && s.ascii && s.word).toBeTruthy();
    }
  });

  it("the phone's colours are opaque unless the token is translucent", () => {
    // Xcode reads a component with no decimal point as 0-255, so alpha "1"
    // would be 1/255. Alpha must be a 0-1 float that matches the token.
    const sets = Object.entries(render(tokens) as Record<string, string>).filter(([p]) => p.endsWith(".colorset/Contents.json"));
    const source = (rel: string): [string, string] => {
      const [dir, key] = rel.split("/");
      const L = tokens.schemes.light as Record<string, unknown>;
      const D = tokens.schemes.dark as Record<string, unknown>;
      if (dir === "presence") {
        const i = Number(key.replace("presence", "")) - 1;
        return [(L.presence as string[])[i], (D.presence as string[])[i]];
      }
      if (dir === "status" || dir === "diff") {
        return [(L[dir] as Record<string, string>)[key], (D[dir] as Record<string, string>)[key]];
      }
      const fam = tokens.themes[dir as keyof typeof tokens.themes] as unknown as Record<"light" | "dark", Record<string, string>>;
      return [fam.light[key], fam.dark[key]];
    };
    expect(sets.length).toBeGreaterThan(50);
    let opaque = 0;
    for (const [path, body] of sets) {
      const rel = path.split("LigusTokens.xcassets/")[1].replace(".colorset/Contents.json", "");
      const [light, dark] = source(rel);
      const set = JSON.parse(body);
      for (const entry of set.colors) {
        const alpha: string = entry.color.components.alpha;
        expect(alpha, `${rel} alpha`).toMatch(/^[01]\.\d{3}$/);
        const want = parse(entry.appearances ? dark : light).a;
        expect(Number(alpha), `${rel} alpha`).toBeCloseTo(want, 3);
        if (want === 1) {
          expect(alpha, `${rel} alpha`).toBe("1.000");
          opaque++;
        }
      }
    }
    expect(opaque).toBeGreaterThan(100);
  });
});

describe("token validator", () => {
  it("the shipped tokens pass every contrast and colour-vision check", () => {
    const report = validate(tokens);
    expect(report.failures).toEqual([]);
    expect(report.checks.length).toBeGreaterThan(500);
  });

  it("fails when warn matches a palette's accent-warm (they mean different things)", () => {
    const bad = clone();
    bad.schemes.light.status.warn = bad.themes.pollen.light.accentWarm;
    expect(validate(bad).failures.some((f) => f.includes("pollen/light: warn vs accent-warm (normal)"))).toBe(true);
  });

  it("fails when two status colours collapse for a colour-blind reader", () => {
    const bad = clone();
    // Same lightness and chroma, red against green: fine with normal vision,
    // nearly identical with deuteranopia.
    bad.schemes.light.status.ok = "oklch(0.55 0.13 140)";
    bad.schemes.light.status.danger = "oklch(0.55 0.13 30)";
    const fails = validate(bad).failures;
    expect(fails.some((f) => f.startsWith("light: ok vs danger (deuteranopia)"))).toBe(true);
    expect(fails.some((f) => f.startsWith("light: ok vs danger (normal)"))).toBe(false);
  });

  it("fails on low-contrast status inks and out-of-gamut colours", () => {
    const bad = clone();
    bad.schemes.dark.status.info = "oklch(0.3 0.09 248)";
    bad.schemes.dark.presence[0] = "oklch(0.76 0.3 255)";
    const fails = validate(bad).failures;
    expect(fails.some((f) => f.includes("info glyph on canvas"))).toBe(true);
    expect(fails.some((f) => f.includes("dark presence-1: inside sRGB"))).toBe(true);
  });

  it("simulates colour-vision deficiencies sensibly", () => {
    const red: [number, number, number] = [255, 0, 0];
    const white: [number, number, number] = [255, 255, 255];
    expect(simulate(white, "deuteranopia")).toEqual(white);
    // Protanopes see red as much darker.
    expect(contrast(simulate(red, "protanopia"), [0, 0, 0])).toBeLessThan(contrast(red, [0, 0, 0]));
  });
});
