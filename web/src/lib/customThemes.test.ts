import { beforeEach, describe, expect, it } from "vitest";
import schema from "../../../docs-site/docs/schemas/ligus-theme.v1.json";
import {
  ACTIVE_KEY,
  ALL_TOKENS,
  MAX_STORED_THEMES,
  MAX_THEME_BYTES,
  STORAGE_KEY,
  addCustomTheme,
  applySchemeOverrides,
  customThemes,
  exportFileName,
  exportThemeJson,
  exportThemeToml,
  parseColor,
  parseThemeFile,
  readToml,
  reloadCustomThemes,
  removeCustomTheme,
  renameCustomTheme,
  resolveCustom,
  setActiveCustomTheme,
} from "./customThemes";
import { applyActiveTheme } from "./theme";
import { PALETTES } from "./tokens.generated";

const TOML = `# Heather, on a light background
format = "ligus.theme.v1"
name = "Heather"
extends = "meadow"

[light]
canvas = "#fbf7fb"
ink = "#2a1f2b"
accentCool = "oklch(0.5 0.12 330)"
ok = "rgb(0, 110, 50)"
line = "rgba(40, 20, 40, 0.12)"

[dark]
ink = "#f2e8f2"
border = "#6a5a6a"
`;

const JSON_SAME = JSON.stringify({
  format: "ligus.theme.v1",
  name: "Heather",
  extends: "meadow",
  light: {
    canvas: "#fbf7fb",
    ink: "#2a1f2b",
    accentCool: "oklch(0.5 0.12 330)",
    ok: "rgb(0, 110, 50)",
    line: "rgba(40, 20, 40, 0.12)",
  },
  dark: { ink: "#f2e8f2", border: "#6a5a6a" },
});

const ok = (text: string, name = "x.toml") => {
  const r = parseThemeFile(text, name);
  if (!r.ok) throw new Error(r.errors.join("\n"));
  return r;
};
const errs = (text: string) => {
  const r = parseThemeFile(text, "x.toml");
  expect(r.ok, `should be refused:\n${text}`).toBe(false);
  return r.ok ? [] : r.errors;
};

describe("colour grammar", () => {
  it("accepts hex, rgb(), rgba(), the space syntax and oklch()", () => {
    expect(parseColor("#abc")).toMatchObject({ css: "rgb(170,187,204)" });
    expect(parseColor("#AABBCC")).toMatchObject({ css: "rgb(170,187,204)" });
    expect(parseColor("#aabbcc80")).toMatchObject({ css: "rgba(170,187,204,0.502)" });
    expect(parseColor(" rgb(1, 2, 3) ")).toMatchObject({ css: "rgb(1,2,3)" });
    expect(parseColor("RGBA(1,2,3,.5)")).toMatchObject({ css: "rgba(1,2,3,0.5)" });
    expect(parseColor("rgb(1 2 3 / 0.25)")).toMatchObject({ css: "rgba(1,2,3,0.25)" });
    // oklch() is applied as rgb(): the editor, terminal and title bar read it back.
    expect(parseColor("oklch(0.2376 0.0267 76.06)")).toMatchObject({ css: "rgb(38,29,16)" });
    expect(parseColor("OKLCH(23.76% 0.0267 76.06 / 0.5)")).toMatchObject({ css: "rgba(38,29,16,0.5)" });
  });

  it("refuses anything that isn't one colour", () => {
    const attempts = [
      "red",
      "red; } body { background: url(x) }",
      "url(javascript:alert(1))",
      "url(https://example.com/x.png)",
      "var(--ligus-ink)",
      "expression(alert(1))",
      "calc(1px + 2px)",
      "#fff;",
      "#fff }",
      "#fff\n}",
      "#ffff",
      "#ggg",
      "rgb(1,2,3);color:red",
      "rgb(1,2,3)) body{",
      "rgb(256, 0, 0)",
      "rgb(1, 2, 3, 2)",
      "rgb(-1, 2, 3)",
      "rgb(1.5, 2, 3)",
      "oklch(1.2 0.1 20)",
      "oklch(0.5 0.9 20)",
      "oklch(0.5 0.1 400)",
      "oklch(0.5 0.1 20) </style><script>",
      "</style>",
      "inherit",
      "",
      "#".padEnd(65, "f"),
      "rgb(" + " ".repeat(70) + "1,2,3)",
    ];
    for (const a of attempts) expect("error" in parseColor(a), a).toBe(true);
    expect("error" in parseColor(12)).toBe(true);
    expect("error" in parseColor(["#fff"])).toBe(true);
    expect("error" in parseColor({ r: 1 })).toBe(true);
  });

  it("only ever produces a colour function built from numbers", () => {
    for (const v of ["#123", "#12345678", "rgb(1 2 3)", "oklch(0.3 0.2 100 / .4)"]) {
      const c = parseColor(v);
      expect("css" in c && /^(rgb|rgba|oklch)\([0-9., /]+\)$/.test(c.css)).toBe(true);
    }
  });
});

describe("theme files", () => {
  it("reads TOML, and the same file as JSON gives the same theme", () => {
    const a = ok(TOML);
    const b = ok(JSON_SAME, "x.json");
    expect(a.theme).toEqual(b.theme);
    expect(a.theme.name).toBe("Heather");
    expect(a.theme.extends).toBe("meadow");
    expect(a.theme.light.canvas).toBe("rgb(251,247,251)");
    expect(a.theme.dark.border).toBe("rgb(106,90,106)");
  });

  it("names a theme after its file when the file doesn't", () => {
    expect(ok(`[dark]\nink = "#eee"`, "/tmp/Dusk.toml").theme.name).toBe("Dusk");
    expect(ok(`[dark]\nink = "#eee"`, "Dusk.toml").theme.extends).toBe("pollen");
  });

  it("gives line numbers for bad colours", () => {
    const e = errs(`name = "x"\n\n[light]\nink = "#fff"\ncanvas = "nope"\n`);
    expect(e).toEqual([expect.stringMatching(/^line 5: light\.canvas .*nope/)]);
    const j = parseThemeFile(`{\n  "light": {\n    "ink": "url(x)"\n  }\n}`, "x.json");
    expect(j.ok).toBe(false);
    if (!j.ok) expect(j.errors[0]).toMatch(/^line 3: light\.ink/);
  });

  it("warns about unknown keys and keeps the theme", () => {
    const r = ok(`author = "me"\n[light]\nink = "#111"\nsparkle = "#fff"\n[extra]\nx = "y"`);
    expect(r.warnings.join("\n")).toMatch(/line 1: unknown key "author"/);
    expect(r.warnings.join("\n")).toMatch(/line 4: unknown colour light\."sparkle"/);
    expect(r.warnings.join("\n")).toMatch(/unknown table "extra"/);
  });

  it("accepts lt's tokens without a warning", () => {
    const r = ok(`[dark]\ninkMuted = "#999"\nborder = "#666"\nselection = "#333"\nagent = "#c9f"`);
    expect(r.warnings).toEqual([]);
  });

  it("refuses odd structures", () => {
    expect(errs(`[light]\nink = 12`)[0]).toMatch(/line 2/);
    expect(errs(`[light]\nink = ["#fff"]`)[0]).toMatch(/line 2/);
    expect(errs(`[light]\nink = { r = 1 }`)[0]).toMatch(/line 2/);
    expect(errs(`[light.deep]\nink = "#fff"`)[0]).toMatch(/line 1/);
    expect(errs(`light = "#fff"`)[0]).toMatch(/light must be a table/);
    expect(errs(`[light]\nink = """#fff"""`)[0]).toMatch(/multi-line/);
    expect(errs(`[light]\nink = "#fff`)[0]).toMatch(/isn't closed/);
    expect(errs(`[light]\nink = "#fff" extra`)[0]).toMatch(/unexpected text/);
    expect(errs(`name = "x"`)[0]).toMatch(/needs a \[light\] or a \[dark\]/);
    expect(errs(`extends = "midnight"\n[dark]\nink = "#fff"`)[0]).toMatch(/extends must be one of/);
    expect(errs(`format = "v2"\n[dark]\nink = "#fff"`)[0]).toMatch(/format must be/);
    expect(errs(`name = "a\\u0007b"\n[dark]\nink = "#fff"`)[0]).toMatch(/control characters/);
    expect(errs(`name = "${"n".repeat(41)}"\n[dark]\nink = "#fff"`)[0]).toMatch(/longer than 40/);
    for (const j of ["[]", '"x"', "12", "null", '{"light": ["#fff"]}', '{"light": "#fff"}', "{bad json"]) {
      expect(parseThemeFile(j, "x.json").ok, j).toBe(false);
    }
    expect(parseThemeFile(`{"light": {"ink": {"nested": "#fff"}}}`, "x.json").ok).toBe(false);
  });

  it("refuses duplicate keys and tables", () => {
    expect(errs(`[light]\nink = "#fff"\nink = "#000"`)[0]).toMatch(/line 3: ink is set twice/);
    expect(errs(`[light]\nink = "#fff"\n[light]\ncanvas = "#000"`)[0]).toMatch(/line 3: \[light\] appears twice/);
    expect(errs(`name = "a"\nname = "b"\n[dark]\nink = "#fff"`)[0]).toMatch(/line 2/);
  });

  it("reads dotted keys and escapes the way TOML does", () => {
    const r = ok(`light.ink = "#111"\n"dark".'canvas' = '#000'\nname = "Caf\\u00e9 \\"night\\""`);
    expect(r.theme.light.ink).toBe("rgb(17,17,17)");
    expect(r.theme.dark.canvas).toBe("rgb(0,0,0)");
    expect(r.theme.name).toBe('Café "night"');
    expect(() => readToml(`a = "\\q"`)).toThrow(/bad escape/);
  });

  it("injection attempts in values are refused, wherever they are", () => {
    const vals = [
      "red; } body { background: url(x) }",
      "url(javascript:alert(1))",
      "var(--x)",
      "expression(alert(1))",
      "calc(100% - 1px)",
      "#fff;",
      "#fff\\n}",
      "#fff }",
      "</style><img src=x onerror=alert(1)>",
    ];
    for (const v of vals) {
      const toml = `[light]\naccentCool = ${JSON.stringify(v)}`;
      expect(parseThemeFile(toml, "x.toml").ok, toml).toBe(false);
      expect(parseThemeFile(JSON.stringify({ dark: { ink: v } }), "x.json").ok, v).toBe(false);
    }
  });

  it("prototype keys are plain unknown keys", () => {
    const j = parseThemeFile(`{"__proto__": {"polluted": "yes"}, "constructor": "x", "light": {"__proto__": "#fff", "ink": "#111"}}`, "x.json");
    expect(j.ok).toBe(true);
    if (j.ok) {
      expect(j.warnings.join("\n")).toMatch(/__proto__/);
      expect(Object.keys(j.theme.light)).toEqual(["ink"]);
    }
    const t = parseThemeFile(`constructor = "x"\n[light]\n__proto__ = "#fff"\ntoString = "#000"`, "x.toml");
    expect(t.ok).toBe(true);
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
  });

  it("refuses a huge file before reading it, and an overlong value", () => {
    const big = `[light]\n# ${"x".repeat(MAX_THEME_BYTES)}\nink = "#fff"`;
    expect(errs(big)[0]).toMatch(/larger than 64 KB/);
    expect(errs(`[light]\nink = "${"#".padEnd(80, "f")}"`)[0]).toMatch(/longer than 64/);
    // Many lines just under the limit still parse quickly.
    const many = "[light]\n" + "# c\n".repeat(15000) + 'ink = "#111"\n';
    const t0 = Date.now();
    expect(parseThemeFile(many, "x.toml").ok).toBe(true);
    expect(Date.now() - t0).toBeLessThan(1000);
  });

  it("warns about low contrast and keeps the theme", () => {
    const r = ok(`[light]\ncanvas = "#ffffff"\nink = "#dddddd"\nwarn = "#ffee00"\n`);
    expect(r.warnings.join("\n")).toMatch(/light: ink on canvas is 1\.\d+:1/);
    expect(r.warnings.join("\n")).toMatch(/light: warn on canvas/);
    // A file that only sets readable colours draws no warnings about the base palette.
    expect(ok(`[light]\nink = "#111111"`).warnings).toEqual([]);
  });
});

describe("resolving and applying", () => {
  it("fills missing tokens from extends, per appearance", () => {
    const { theme } = ok(TOML);
    const light = resolveCustom(theme, "light");
    expect(light.palette.canvas).toBe("rgb(251,247,251)");
    expect(light.palette.panel).toBe(PALETTES.meadow.light.panel);
    expect(light.scheme["--ligus-success"]).toBe("rgb(0,110,50)");
    const dark = resolveCustom(theme, "dark");
    expect(dark.palette).toEqual({ ...PALETTES.meadow.dark, ink: "rgb(242,232,242)" });
    // lt-only tokens never reach the desktop's properties.
    expect(Object.keys(dark.scheme)).toEqual([]);
  });

  it("sets only --ligus-* properties, with canonical values", () => {
    const { theme } = ok(TOML);
    applyActiveTheme("meadow", "light", { ...theme, id: "t1" });
    const root = document.documentElement.style;
    expect(root.getPropertyValue("--ligus-canvas")).toBe("rgb(251,247,251)");
    expect(root.getPropertyValue("--ligus-success")).toBe("rgb(0,110,50)");
    applySchemeOverrides({ color: "red", "--ligus-ink": "rgb(1,2,3)" });
    expect(root.getPropertyValue("color")).toBe("");
    applyActiveTheme("pollen", "light", null);
    expect(root.getPropertyValue("--ligus-canvas")).toBe(PALETTES.pollen.light.canvas);
  });

  it("exports a full file that imports back to the same colours", () => {
    const toml = exportThemeToml({ name: "Pollen (mine)", extends: "pollen", light: {}, dark: {} });
    const back = ok(toml);
    expect(back.warnings.filter((w) => !w.includes(" on "))).toEqual([]);
    expect(back.theme.extends).toBe("pollen");
    for (const k of ["canvas", "ink", "accentCool", "sidebarTop", "ok", "diffAddBg"] as const) {
      expect(back.theme.light[k]).toBeDefined();
      expect(back.theme.dark[k]).toBeDefined();
    }
    expect(resolveCustom(back.theme, "light").palette).toEqual(PALETTES.pollen.light);
    const json = ok(exportThemeJson({ name: "Pollen (mine)", extends: "pollen", light: {}, dark: {} }), "x.json");
    expect(json.theme).toEqual(back.theme);
    // Every exported key is a known token.
    for (const k of Object.keys(back.theme.light)) expect(ALL_TOKENS).toContain(k);
  });

  it("export file names are plain", () => {
    expect(exportFileName("Pollen (mine)")).toBe("pollen-mine.toml");
    expect(exportFileName("../../x")).toBe("x.toml");
    expect(exportFileName("…")).toBe("ligus-theme.toml");
  });
});

describe("storage on this device", () => {
  beforeEach(() => {
    localStorage.clear();
    reloadCustomThemes();
  });

  it("round-trips, renames, removes and picks the active theme", () => {
    const { theme } = ok(TOML);
    const t = addCustomTheme(theme);
    expect(customThemes().active?.id).toBe(t.id);
    reloadCustomThemes();
    expect(customThemes().themes).toEqual([t]);
    expect(customThemes().active?.id).toBe(t.id);
    expect(renameCustomTheme(t.id, "  Dusk ")).toBe(true);
    expect(renameCustomTheme(t.id, "")).toBe(false);
    expect(renameCustomTheme(t.id, "x".repeat(41))).toBe(false);
    reloadCustomThemes();
    expect(customThemes().themes[0].name).toBe("Dusk");
    setActiveCustomTheme(null);
    expect(customThemes().active).toBeNull();
    setActiveCustomTheme("not-there");
    expect(customThemes().active).toBeNull();
    setActiveCustomTheme(t.id);
    removeCustomTheme(t.id);
    expect(customThemes()).toEqual({ themes: [], active: null });
    expect(localStorage.getItem(ACTIVE_KEY)).toBeNull();
  });

  it("keeps at most the cap", () => {
    const { theme } = ok(TOML);
    for (let i = 0; i < MAX_STORED_THEMES; i++) addCustomTheme(theme);
    expect(() => addCustomTheme(theme)).toThrow(/Remove one first/);
  });

  it("drops corrupted or tampered entries instead of applying them", () => {
    const good = { id: "abc", name: "Fine", extends: "pollen", light: { ink: "rgb(1,2,3)" }, dark: {} };
    const bad = [
      { ...good, id: "b1", light: { ink: "red; } body { background: url(x) }" } },
      { ...good, id: "b2", light: { "--x": "rgb(1,2,3)" } },
      { ...good, id: "b3", extends: "midnight" },
      { ...good, id: "B 4" },
      { ...good, id: "b5", name: "\u0007" },
      { ...good, id: "b6", light: "rgb(1,2,3)" },
      "nope",
      null,
    ];
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ v: 1, themes: [good, ...bad, good] }));
    localStorage.setItem(ACTIVE_KEY, "b1");
    reloadCustomThemes();
    expect(customThemes().themes.map((t) => t.id)).toEqual(["abc"]);
    expect(customThemes().active).toBeNull();
    for (const raw of ["{not json", JSON.stringify({ v: 2, themes: [good] }), JSON.stringify([good]), "x".repeat(400_000)]) {
      localStorage.setItem(STORAGE_KEY, raw);
      reloadCustomThemes();
      expect(customThemes().themes).toEqual([]);
    }
  });
});

describe("the shared schema", () => {
  it("lists exactly the tokens both apps know", () => {
    expect(Object.keys(schema.$defs.scheme.properties).sort()).toEqual([...ALL_TOKENS].sort());
    expect([...schema.properties.extends.enum].sort()).toEqual(Object.keys(PALETTES).sort());
    const pattern = new RegExp(schema.$defs.color.pattern);
    for (const v of ["#abc", "rgb(1, 2, 3)", "rgba(1,2,3,.5)", "rgb(1 2 3 / 0.5)", "oklch(50% 0.1 200 / 0.5)"]) {
      expect(pattern.test(v), v).toBe(true);
      expect("css" in parseColor(v), v).toBe(true);
    }
    for (const v of ["url(x)", "#fff;", "var(--x)"]) expect(pattern.test(v), v).toBe(false);
  });
});

describe("review follow-ups", () => {
  it("refuses names with bidi controls, lone surrogates or only invisible characters", () => {
    for (const name of ["\u202egnp.exe", "a\u2066b", "\ud800x", "\u200b", "\ufeff \u200d"]) {
      const r = parseThemeFile(JSON.stringify({ name, dark: { ink: "#fff" } }), "x.json");
      expect(r.ok, JSON.stringify(name)).toBe(false);
    }
  });

  it("finds JSON lines fast and in the right table", () => {
    const pad = "\n".repeat(30000);
    const keys = Array.from({ length: 2000 }, (_, i) => `"k${i}":"#000"`).join(",\n");
    const text = `{"name": "light dark", ${pad}"light": {\n${keys},\n"ink": "nope"\n}}`;
    const t0 = performance.now();
    const r = parseThemeFile(text, "x.json");
    expect(performance.now() - t0).toBeLessThan(500);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.errors.join("\n")).toMatch(/line 3\d{4}: /);
  });
});
