#!/usr/bin/env node
// Screenshots of the Ligus desktop UI for the docs, the website and the
// READMEs, from the web mock (web/dev/mock: canned demo data, no backend).
//
//   node docs-site/tools/screenshots/capture-desktop.mjs [--out DIR] [--only a,b] [--port 5310]
//
// Run it from the root of a checkout of the release being documented
// (for example `git worktree add ../ligus-v0.3.0 v0.3.0`), after
// `bun install` in web/. It starts `vite --config vite.mock.config.ts`,
// drives Chrome with playwright-core (a web devDependency) at 1440x900, in
// the Pollen palette light and dark (plus Harbor for the themes shots), and
// writes for every view:
//
//   DIR/png/desktop-<view>-<mode>.png       2x, pngquant'd   (docs)
//   DIR/png/desktop-<view>-<mode>@1x.png    1x, pngquant'd   (READMEs)
//   DIR/webp/desktop-<view>-<mode>.webp     1x               (website)
//   DIR/webp/desktop-<view>-<mode>@2x.webp  2x               (website)
//
// Needs Google Chrome (or CHROME=/path/to/chrome), bun, cwebp and pngquant
// (brew install webp pngquant). Nothing in the repo is modified: the demo
// data's names are rewritten in the browser (see REWRITES) so no shot shows
// a real person, host or account.
import { spawn, execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : dflt;
};
const ROOT = process.cwd();
const WEB = path.join(ROOT, "web");
if (!fs.existsSync(path.join(WEB, "vite.mock.config.ts"))) {
  console.error("run this from the root of a Ligus checkout (web/vite.mock.config.ts not found)");
  process.exit(2);
}
const OUT = path.resolve(opt("out", "screenshots-out"));
const PORT = Number(opt("port", "5310"));
const ONLY = opt("only", "")?.split(",").filter(Boolean);
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
for (const tool of ["cwebp", "pngquant"]) {
  try {
    execFileSync("which", [tool], { stdio: "ignore" });
  } catch {
    console.error(`${tool} not found (brew install webp pngquant)`);
    process.exit(3);
  }
}
const require = createRequire(path.join(WEB, "package.json"));
const { chromium } = require("playwright-core");

// Demo data stays demo data: names that read as real people or machines are
// swapped, and one tool result that shows a failure in the hero is shown
// succeeding. Applied to the mock's modules as the browser loads them.
const REWRITES = [
  [/zazulam/g, "mike-dev"],
  [/Zaz/g, "Mike"],
  [/zaz/g, "mike"],
  [/home-server/g, "serve box"],
  [/Ana's iPhone/g, "Mike's iPhone"],
  [/relay\.langstroth\.ai/g, "relay.example.org"],
  [/content: "error: patch did not apply cleanly at src\/scheduler\.rs:41",\s*isError: true/g, 'content: "Applied: src/scheduler.rs (+4 -0)", isError: false'],
  [/(turns: 3,\s*costUsd: 0?\.42,\s*error: )true/g, "$1false"],
];

const rail = (p) => p.locator('aside[data-tour="right-rail"]');
const pane = (name) => async (p) => {
  await p.getByRole("button", { name: new RegExp(`^${name}( \\d+)?$`) }).last().click();
};
const settings = (tab) => async (p) => {
  await p.keyboard.press("Meta+,");
  await p.waitForTimeout(400);
  await p.getByRole("dialog").getByRole("button", { name: tab, exact: true }).last().click();
};
const palette = async (p, text) => {
  await p.keyboard.press("Meta+k");
  await p.waitForTimeout(400);
  await p.keyboard.type(text, { delay: 20 });
};
const reviewWithVotes = async (p) => {
  await pane("Review")(p);
  await p.waitForTimeout(500);
  const r = rail(p);
  await r.getByRole("button", { name: /^How should we schedule/ }).first().click();
  await p.waitForTimeout(300);
  await r.getByRole("button", { name: /^Run cargo fmt/ }).first().click();
  await p.waitForTimeout(300);
  await r.getByRole("button", { name: /^Cold-day floor/ }).first().click();
  await p.waitForTimeout(400);
  await r.getByRole("button", { name: "Reject" }).first().scrollIntoViewIfNeeded();
};

const onboardingStep = (n) => async (p) => {
  for (let i = 1; i < n; i++) {
    await p.getByRole("button", { name: "Next", exact: true }).click();
    await p.waitForTimeout(600);
  }
};
const settingsDialog = [260, 110, 1180, 790];

// view: [navigate, extra query, shots {file name: crop [x0, y0, x1, y1] in
// CSS px, or null for the whole 1440x900 window}]. The file names are the
// ones the docs (docs-site/docs/ligus/images/desktop-<name>-<mode>.png), the
// website (public/shots/desktop-<name>-<mode>[@2x].webp) and the READMEs use.
const VIEWS = {
  hero: [reviewWithVotes, "", { overview: null, review: [1100, 56, 1440, 876] }],
  chat: [async () => {}, "", { chat: null }],
  tools: [pane("Agents & tools"), "", { "tools-window": null, tools: [1100, 56, 1440, 520] }],
  inbox: [async (p) => p.getByRole("button", { name: /^Inbox/ }).first().click(), "&inbox=1", { "inbox-window": null, inbox: [500, 0, 1260, 650] }],
  people: [
    async (p) => {
      await pane("People")(p);
      await p.waitForTimeout(500);
      await p.locator("aside").getByText("Theo", { exact: true }).first().click({ button: "right" });
    },
    "",
    { "people-window": null, "people-menu": [1100, 56, 1440, 560] },
  ],
  invite: [
    async (p) => {
      await pane("People")(p);
      await p.waitForTimeout(400);
      await p.getByRole("button", { name: "Invite", exact: true }).first().click();
    },
    "",
    { invite: [510, 322.5, 930, 577.5] },
  ],
  appearance: [settings("Appearance"), "", { "appearance-window": null, appearance: settingsDialog }],
  "settings-profile": [
    async (p) => {
      await settings("Profile")(p);
      await p.waitForTimeout(500);
      await p.getByRole("button", { name: "Add a computer" }).scrollIntoViewIfNeeded();
    },
    "",
    { "settings-profile": settingsDialog },
  ],
  // Linked devices: a member that is you on another computer, and My
  // devices with revoked ones folded away (?devices=1).
  "people-link-hint": [
    async (p) => {
      await pane("People")(p);
      await p.waitForTimeout(500);
      await p.locator("aside").getByText(/^\w+ \(lt\)$/).first().click({ button: "right" });
    },
    "&devices=1",
    { "people-link-hint-window": null, "people-link-hint": [760, 56, 1440, 560] },
  ],
  "palette-add-computer": [async (p) => palette(p, "link"), "&devices=1", { "palette-add-computer": [464, 108, 976, 574] }],
  "settings-devices": [
    async (p) => {
      await settings("Profile")(p);
      await p.waitForTimeout(500);
      await p.getByRole("button", { name: /^Revoked \(/ }).click();
      await p.waitForTimeout(300);
      await p.getByRole("button", { name: /^Revoked \(/ }).scrollIntoViewIfNeeded();
    },
    "&devices=1",
    { "settings-devices": settingsDialog },
  ],
  "settings-devices-collapsed": [
    async (p) => {
      await settings("Profile")(p);
      await p.waitForTimeout(500);
      await p.getByRole("link", { name: "My devices" }).click();
    },
    "&devices=1",
    { "settings-devices-collapsed": settingsDialog },
  ],
  "add-computer-from-people": [
    async (p) => {
      await pane("People")(p);
      await p.waitForTimeout(500);
      await p.locator("aside").getByText(/^\w+ \(lt\)$/).first().click({ button: "right" });
      await p.getByRole("menuitem", { name: /another computer/ }).click();
    },
    "&devices=1",
    { "add-computer-from-people": null },
  ],
  "settings-models": [settings("Models & runtimes"), "", { "settings-models": settingsDialog }],
  "settings-tools": [settings("Tools & MCP"), "", { "settings-tools": settingsDialog }],
  "settings-sync": [settings("Team sync"), "", { "settings-sync": settingsDialog }],
  "settings-sandbox": [settings("Sandbox"), "", { "settings-sandbox": settingsDialog }],
  workflows: [pane("Workflows"), "", { workflows: null }],
  "onboarding-identity": [onboardingStep(1), "&scenario=onboarding", { "onboarding-identity": [476, 122, 964, 779] }],
  "onboarding-agent": [onboardingStep(3), "&scenario=onboarding", { "onboarding-agent": [476, 60, 964, 830] }],
  "onboarding-team": [onboardingStep(4), "&scenario=onboarding", { "onboarding-team": [476, 151, 964, 716] }],
  "onboarding-appearance": [onboardingStep(5), "&scenario=onboarding", { "onboarding-appearance": [476, 122, 964, 779] }],
  // The guided setup flows (Ligus 0.3.0 and later).
  "setup-palette": [async (p) => palette(p, "set up"), "", { "setup-palette": [464, 108, 976, 574] }],
  "setup-agent": [
    async (p) => {
      await palette(p, "Set up an agent");
      await p.waitForTimeout(300);
      await p.keyboard.press("Enter");
      await p.waitForTimeout(700);
      await p.getByRole("dialog").getByText("Claude Code", { exact: false }).first().click();
    },
    "",
    { "setup-agent-window": null, "setup-agent": [1000, 0, 1440, 900] },
  ],
};
// Pollen (the default palette) in both modes for every view; Harbor for the
// themes shots.
const RUNS = [
  ["pollen", "light", Object.keys(VIEWS)],
  ["pollen", "dark", Object.keys(VIEWS)],
  ["harbor", "light", ["hero", "appearance"]],
  ["harbor", "dark", ["hero", "appearance"]],
];

const vite = spawn("bunx", ["vite", "--config", "vite.mock.config.ts", "--port", String(PORT), "--strictPort"], { cwd: WEB, stdio: "ignore" });
process.on("exit", () => vite.kill());
const base = `http://localhost:${PORT}/`;
for (let i = 0; ; i++) {
  try {
    if ((await fetch(base)).ok) break;
  } catch {}
  if (i > 60) throw new Error("the mock server did not start");
  await new Promise((r) => setTimeout(r, 500));
}

fs.mkdirSync(path.join(OUT, "raw"), { recursive: true });
fs.mkdirSync(path.join(OUT, "png"), { recursive: true });
fs.mkdirSync(path.join(OUT, "webp"), { recursive: true });
const browser = await chromium.launch({ executablePath: CHROME });
const shots = [];
for (const [theme, mode, views] of RUNS) {
  for (const scale of [1, 2]) {
    const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: scale, colorScheme: mode });
    await ctx.addInitScript(([t, m]) => {
      localStorage.setItem("ligus.theme", t);
      localStorage.setItem("ligus.appearance", m);
    }, [theme, mode]);
    await ctx.route(/\/dev\/mock\/[^/?]+\.ts/, async (route) => {
      let res;
      for (let i = 0; ; i++) {
        try {
          res = await route.fetch();
          break;
        } catch (e) {
          if (i > 4) throw e;
          await new Promise((r) => setTimeout(r, 500));
        }
      }
      let body = await res.text();
      for (const [re, to] of REWRITES) body = body.replace(re, to);
      await route.fulfill({ response: res, body });
    });
    for (const view of views.filter((v) => !ONLY?.length || ONLY.includes(v))) {
      const [go, query, outputs] = VIEWS[view];
      const p = await ctx.newPage();
      p.setDefaultTimeout(10000);
      await p.goto(`${base}?shot=1${query}`, { waitUntil: "networkidle" });
      await p.waitForTimeout(900);
      try {
        await p.getByRole("button", { name: "Dismiss suggestion" }).first().click({ timeout: 1500 });
      } catch {}
      try {
        await go(p);
      } catch (e) {
        console.error(`${view}: navigation failed: ${e.message.split("\n")[0]}`);
      }
      await p.waitForTimeout(800);
      const suffix = theme === "pollen" ? mode : `${theme}-${mode}`;
      const tag = scale === 2 ? "@2x" : "@1x";
      const one = async (name, clip) => {
        const file = path.join(OUT, "raw", `desktop-${name}-${suffix}${tag}.png`);
        await p.screenshot({ path: file, ...(clip ? { clip: { x: clip[0], y: clip[1], width: clip[2] - clip[0], height: clip[3] - clip[1] } } : {}) });
        shots.push(file);
      };
      for (const [name, box] of Object.entries(outputs)) await one(name, box);
      await p.close();
    }
    await ctx.close();
  }
}
await browser.close();
vite.kill();

for (const raw of shots) {
  const name = path.basename(raw).replace(/@[12]x\.png$/, "");
  const two = raw.endsWith("@2x.png");
  const png = path.join(OUT, "png", two ? `${name}.png` : `${name}@1x.png`);
  const webp = path.join(OUT, "webp", two ? `${name}@2x.webp` : `${name}.webp`);
  fs.copyFileSync(raw, png);
  execFileSync("pngquant", ["--quality=65-90", "--speed", "1", "--force", "--ext", ".png", png]);
  execFileSync("cwebp", ["-quiet", "-q", "82", "-m", "6", raw, "-o", webp]);
}
console.log(`${shots.length} screenshots in ${OUT}/png and ${OUT}/webp`);
process.exit(0);
