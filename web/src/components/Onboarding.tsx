import { useEffect, useState } from "react";
import {
  setDisplayName,
  setAvatar,
  myProfile,
  setGitEmail,
  pickWorkspaceFolder,
  addWorkspaceToList,
  setWorkspaceRoot,
  addRuntime,
  setClaudeCodeModel,
  setDefaultModel,
  listClaudeCodeModels,
  updateConnectionSettings,
  getConnectionSettings,
  detectEnvironment,
  openExternal,
  joinWorkspace,
  redeemShortCode,
  probeRelay,
  probeRelayAt,
  probeProvider,
  type RuntimeTestResult,
  type EnvDetectDto,
  type ClaudeModelOption,
  type ClaudePermissionMode,
  type RelayProbeDto,
} from "@/lib/ipc";
import { LigusBrandIcon } from "@/components/LigusBrand";
import { RelayPicker } from "@/components/RelayPicker";
import { relayUrlFor, type RelayChoice } from "@/lib/communityRelay";
import { Avatar } from "@/components/Avatar";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import {
  applyTheme,
  loadMode,
  loadTheme,
  resolvePalette,
  saveMode,
  savePalette,
  type AppearanceMode,
  type ThemeName,
} from "@/lib/theme";

type RuntimeChoice = "claudeCode" | "codex" | "openai" | "anthropic" | "ollama";

// Onboarding's appearance step. Order + labels for the accent-family picker.
const THEME_CHOICES: { name: ThemeName; label: string; blurb: string }[] = [
  { name: "pollen", label: "Pollen", blurb: "Warm honey — the Ligus default" },
  { name: "obsidian", label: "Obsidian", blurb: "True dark — monochrome near-black glass, silver accent" },
  { name: "slate", label: "Slate", blurb: "Cool blue-grey" },
  { name: "studio", label: "Studio", blurb: "Neutral graphite" },
  { name: "harbor", label: "Harbor", blurb: "Ocean blue" },
  { name: "meadow", label: "Meadow", blurb: "Botanical green" },
];

// backgroundColor (not the `background` shorthand) so the global `select` rule's
// chevron background-image survives on the runtime/model dropdowns.
const inputStyle = { borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-panel)" };
const field = "w-full rounded-xl border px-3 py-2.5 text-sm outline-none";

// The Claude Code `--model` value can be a short alias (opus) or a full id
// (claude-fable-5[1m]); the synthesized "Primary Runtime" (default_model) wants
// a full id for its label, so new chats show the same model that was picked.
const CLAUDE_ALIAS_TO_ID: Record<string, string> = {
  "": "claude-sonnet-4-6", // CLI default is usually Sonnet
  sonnet: "claude-sonnet-4-6",
  opus: "claude-opus-4-8",
  haiku: "claude-haiku-4-5-20251001",
};
// Shown if the backend model list can't be read (Claude Code not installed, or
// the CLI cache absent). The CLI always understands these aliases.
const CLAUDE_BASE_MODELS: ClaudeModelOption[] = [
  { value: "", label: "Default (CLI decides — usually Sonnet)", description: null },
  { value: "sonnet", label: "Sonnet", description: null },
  { value: "opus", label: "Opus", description: null },
  { value: "haiku", label: "Haiku", description: null },
];
function claudeDefaultModelFor(value: string): string {
  const v = value.trim();
  if (v in CLAUDE_ALIAS_TO_ID) return CLAUDE_ALIAS_TO_ID[v];
  // A full value like "claude-fable-5[1m]" → drop the context-window suffix.
  return v.replace(/\[[^\]]*\]$/, "");
}

/// First-run wizard: identity → project folder → agent + file access → optional
/// team. Detects what's installed (Claude Code, Ollama, API keys) to default
/// well, and supports any OpenAI-compatible API (OpenAI/OpenRouter/local).
export function Onboarding({ onComplete }: { onComplete: () => void | Promise<void> }) {
  const [step, setStep] = useState(1);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [env, setEnv] = useState<EnvDetectDto | null>(null);

  // Step 1 — identity
  const [name, setName] = useState("");
  const [picture, setPicture] = useState<string | null>(null);
  const [safetyCode, setSafetyCode] = useState<string | null>(null);

  // Step 2 — project
  const [folder, setFolder] = useState<string | null>(null);

  // Step 3 — runtime + access
  const [choice, setChoice] = useState<RuntimeChoice>("claudeCode");
  const [apiKey, setApiKey] = useState("");
  // Agent connectivity check (API providers): verify the key actually answers
  // before Finish, so nobody completes onboarding with a silently-dead agent.
  // `ackFail` lets a user proceed anyway after one clear failure (transient/edge).
  const [agentProbe, setAgentProbe] = useState<RuntimeTestResult | "running" | null>(null);
  const [agentAckFail, setAgentAckFail] = useState(false);
  const agentNeedsKey = choice === "openai" || choice === "anthropic";

  async function probeAgent(): Promise<boolean> {
    setAgentProbe("running");
    try {
      const kind = choice === "anthropic" ? "anthropic" : "openAI";
      const r = await probeProvider(
        kind,
        choice === "openai" ? model.trim() || undefined : undefined,
        apiKey.trim() || undefined,
        choice === "openai" ? baseUrl.trim() || undefined : undefined,
      );
      setAgentProbe(r);
      return r.ok;
    } catch (e) {
      setAgentProbe({ ok: false, latency_ms: 0, reply: "", error: String(e) });
      return false;
    }
  }
  const [baseUrl, setBaseUrl] = useState("https://api.openai.com/v1/chat/completions");
  const [model, setModel] = useState("gpt-4o");
  // Any change to the agent choice/creds invalidates a prior probe result.
  useEffect(() => {
    setAgentProbe(null);
    setAgentAckFail(false);
  }, [choice, apiKey, model, baseUrl]);
  const [claudeModel, setClaudeModel] = useState(""); // "" = CLI default
  const [claudeModels, setClaudeModels] = useState<ClaudeModelOption[]>([]);
  const [claudeCustom, setClaudeCustom] = useState(false);
  const [letEdit, setLetEdit] = useState(true); // Accept edits by default

  // Step 5 — appearance (accent family + light/dark). Applied live for preview
  // and persisted to localStorage; App re-reads both on completion.
  const [theme, setTheme] = useState<ThemeName>(loadTheme);
  const [appearance, setAppearance] = useState<AppearanceMode>(loadMode);
  const pickTheme = (name: ThemeName) => {
    setTheme(name);
    savePalette(name);
    applyTheme(resolvePalette(name, appearance));
  };
  const pickAppearance = (m: AppearanceMode) => {
    setAppearance(m);
    saveMode(m);
    applyTheme(resolvePalette(theme, m));
  };

  // Step 4 — team / relay connection
  type TeamMode = "local" | "join" | "host";
  const [teamMode, setTeamMode] = useState<TeamMode>("local");
  const [inviteCode, setInviteCode] = useState("");
  // "host": which relay to sync through. The community relay is preselected;
  // "own" uses the typed URL. (No relay is the "local" mode.)
  const [relayChoice, setRelayChoice] = useState<RelayChoice>("community");
  const [relayUrl, setRelayUrl] = useState("");
  const hostRelayUrl = relayUrlFor(relayChoice, relayUrl);
  const [accessToken, setAccessToken] = useState("");
  // The community relay is open: a token typed for "own" never goes to it.
  const hostToken = relayChoice === "own" ? accessToken.trim() : "";
  const [probe, setProbe] = useState<RelayProbeDto | null>(null);
  const [testing, setTesting] = useState(false);
  // A connection is "good enough to finish" only when the last probe was ok.
  const connectionOk = probe?.status === "ok";

  const [reChecking, setReChecking] = useState(false);
  // Re-run environment detection — so a user who installs a CLI mid-onboarding
  // can pick it up without restarting the app. Doesn't override an explicit
  // runtime choice the user already made.
  async function reDetect() {
    setReChecking(true);
    try {
      const d = await detectEnvironment();
      setEnv(d);
    } catch {
      /* best-effort */
    } finally {
      setReChecking(false);
    }
  }

  useEffect(() => {
    void (async () => {
      try {
        const d = await detectEnvironment();
        setEnv(d);
        if (d.gitName) setName((n) => n || d.gitName!);
        // Recommend the best available runtime.
        setChoice(d.claudeCode ? "claudeCode" : d.codex ? "codex" : d.openaiEnv ? "openai" : d.ollama ? "ollama" : "anthropic");
      } catch {
        /* detection is best-effort */
      }
      try {
        // The account key and this device's certificate were made silently
        // on first launch; show the safety code that fingerprints them.
        setSafetyCode((await myProfile()).safetyCode);
      } catch {
        /* ignore */
      }
      try {
        const c = await getConnectionSettings();
        setRelayUrl(c.relayUrl);
      } catch {
        /* ignore */
      }
      try {
        setClaudeModels(await listClaudeCodeModels());
      } catch {
        /* ignore — the dropdown falls back to base aliases */
      }
    })();
  }, []);

  async function pickPicture(file: File | undefined) {
    if (!file) return;
    try {
      setPicture(await fileToAvatarDataUrl(file));
    } catch (e) {
      setError(String(e));
    }
  }

  async function pickFolder() {
    try {
      const p = await pickWorkspaceFolder();
      if (p) {
        setFolder(p);
        await addWorkspaceToList(p);
        await setWorkspaceRoot(p);
      }
    } catch (e) {
      setError(String(e));
    }
  }

  const pickMode = (m: TeamMode) => {
    setTeamMode(m);
    setProbe(null);
    setError(null);
  };

  const canNext =
    (step === 1 && name.trim().length > 0) ||
    (step === 2) ||
    (step === 3 && (choice === "claudeCode" || choice === "codex" || choice === "ollama" || apiKey.trim().length > 0)) ||
    // Step 4: local is always fine; join/host must have a verified connection.
    (step === 4 && (teamMode === "local" || connectionOk)) ||
    // Step 5 (appearance) always has a valid selection.
    (step === 5);

  async function next() {
    setError(null);
    setBusy(true);
    try {
      if (step === 1) {
        if (name.trim()) {
          await setDisplayName(name.trim());
          if (env?.gitEmail) await setGitEmail(env.gitEmail);
        }
        if (picture) await setAvatar(picture);
        setStep(2);
      } else if (step === 2) {
        setStep(3);
      } else if (step === 3) {
        // Guarantee a responding agent: for an API provider, verify the key
        // actually answers before advancing. A first failure warns and holds;
        // a second Next (agentAckFail) proceeds anyway so a transient/edge probe
        // failure can't trap the user.
        if (agentNeedsKey && !(typeof agentProbe === "object" && agentProbe?.ok) && !agentAckFail) {
          const ok = await probeAgent();
          if (!ok) {
            setAgentAckFail(true);
            setError(
              "Couldn't reach that provider with this key — @ligus won't answer until it works. Fix the key, or press Continue again to set it up anyway.",
            );
            return;
          }
        }
        await applyRuntime();
        setStep(4);
      } else if (step === 4) {
        await finishStep4();
        setStep(5);
      } else {
        await onComplete();
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function applyRuntime() {
    const c = await getConnectionSettings();
    const permissionMode: ClaudePermissionMode = letEdit ? "runCommands" : "default";
    if (choice === "openai") {
      const id = `openai-${Date.now().toString(36)}`;
      await addRuntime(id, "OpenAI-compatible", "openAI", "remote", baseUrl.trim(), model.trim() || "gpt-4o", true, false);
    } else if (choice === "anthropic") {
      // The `model` field is only shown for the OpenAI-compatible choice, so it
      // still holds its "gpt-4o" default here — don't leak it into an Anthropic
      // runtime; default to Sonnet.
      await addRuntime("anthropic-api", "Anthropic API", "anthropic", "remote", "https://api.anthropic.com/v1/messages", "claude-sonnet-4-6", true, false);
    } else if (choice === "ollama") {
      await addRuntime("ollama-local", "Ollama (local)", "ollama", "local", "http://localhost:11434", model.trim() || "llama3.1", true, false);
    } else if (choice === "claudeCode") {
      // Claude Code uses the built-in runtime; persist the chosen --model.
      await setClaudeCodeModel(claudeModel);
      // The picked model also drives the synthesized "Primary Runtime" the chat
      // header shows and new chats default to — otherwise it stays on the
      // hardcoded Sonnet default and silently overrides the user's pick.
      await setDefaultModel(claudeDefaultModelFor(claudeModel));
    } else if (choice === "codex") {
      // Codex is a subprocess runtime — no key (auth is `codex login`). Empty
      // endpoint = `codex` on PATH; empty model = codex's own default.
      await addRuntime("codex-cli", "Codex", "codex", "local", "", "", true, false);
    }
    const needsKey = choice === "openai" || choice === "anthropic";
    await updateConnectionSettings({
      relayUrl: c.relayUrl,
      room: c.room,
      workspaceKey: null,
      apiKey: needsKey && apiKey.trim() ? apiKey.trim() : null,
      relayAccessToken: null,
      permissionMode,
    });
  }

  const permMode: ClaudePermissionMode = letEdit ? "runCommands" : "default";

  /// Test the relay connection for the current mode, without finishing.
  /// - join: redeem the invite/short code (sets relay+room+key), apply any
  ///   token, then probe the now-configured relay.
  /// - host: probe the entered URL+token *without* saving it yet.
  async function testConnection() {
    setError(null);
    setProbe(null);
    setTesting(true);
    try {
      if (teamMode === "join") {
        const code = inviteCode.trim();
        if (!code) return;
        if (code.toLowerCase().startsWith("hivews1:")) {
          await joinWorkspace(code);
        } else {
          await redeemShortCode(code);
        }
        const c = await getConnectionSettings();
        if (accessToken.trim()) {
          await updateConnectionSettings({
            relayUrl: c.relayUrl,
            room: c.room,
            workspaceKey: null, // preserve the key the invite set
            apiKey: null,
            relayAccessToken: accessToken.trim(),
            permissionMode: permMode,
          });
        }
        setProbe(await probeRelay());
      } else if (teamMode === "host") {
        setProbe(await probeRelayAt(hostRelayUrl, hostToken || null));
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setTesting(false);
    }
  }

  async function finishStep4() {
    const c = await getConnectionSettings();
    if (teamMode === "local") {
      // Clear any relay so we never finish half-configured.
      await updateConnectionSettings({
        relayUrl: "",
        room: c.room,
        workspaceKey: null,
        apiKey: null,
        relayAccessToken: "",
        permissionMode: permMode,
      });
    } else if (teamMode === "host") {
      await updateConnectionSettings({
        relayUrl: hostRelayUrl,
        room: c.room,
        workspaceKey: null,
        apiKey: null,
        relayAccessToken: hostToken,
        permissionMode: permMode,
      });
    }
    // join already applied its settings during testConnection().
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-6" style={{ background: "var(--ligus-canvas)" }}>
      <div className="w-full max-w-md rounded-3xl border p-8" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}>
        <div className="mb-1 flex items-center gap-3">
          <LigusBrandIcon size={40} />
          <h1 className="ligus-display">Welcome to Ligus</h1>
        </div>
        <div className="mb-4 text-sm text-ink-soft">
          A local-first workspace where you and your AI agents build side by side — bring your own
          models, keep your code and keys on your machine. A few quick steps:
        </div>
        <div className="mb-5 flex gap-1.5">
          {[1, 2, 3, 4, 5].map((n) => (
            <div key={n} className="h-1 flex-1 rounded-full" style={{ background: n <= step ? "var(--ligus-accent-cool)" : "var(--ligus-line)" }} />
          ))}
        </div>

        {step === 1 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Who are you?</div>
            <div className="flex items-center gap-3">
              <label className="relative cursor-pointer" data-tip="Add a picture (optional)">
                <Avatar name={name || "You"} url={picture} kind="human" size={48} />
                <input
                  type="file"
                  accept="image/*"
                  className="hidden"
                  onChange={(e) => {
                    const f = e.target.files?.[0];
                    e.target.value = "";
                    void pickPicture(f);
                  }}
                />
              </label>
              <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Your name" className={field} style={inputStyle} />
            </div>
            <div className="text-xs text-ink-soft">
              How teammates see you. The picture is optional and stays small; it is cropped on this device.
            </div>
            <div className="rounded-xl border px-3 py-2.5 text-xs" style={inputStyle}>
              <div className="mb-1 font-medium">Your account on this device</div>
              <div className="text-ink-soft">
                Ligus made an account key on this device. No provider signs you in. Your safety code, which people
                can compare with the code they see for you:
              </div>
              <code className="mt-1 block font-mono text-sm font-bold tracking-wider">{safetyCode ?? "…"}</code>
              <div className="mt-1 text-ink-soft">
                Already use Ligus on another device? Enroll this one later from Settings → Profile → My devices.
              </div>
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Open your project</div>
            <div className="text-xs text-ink-soft">Pick the folder agents should work in (a git repo is ideal).</div>
            <button className="rounded-xl px-3 py-2.5 text-sm font-semibold" style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }} onClick={pickFolder}>
              {folder ? "Choose a different folder…" : "Choose folder…"}
            </button>
            {folder && <div className="truncate rounded-lg border px-3 py-2 font-mono text-xs" style={inputStyle}>✓ {folder}</div>}
            {!folder && <div className="text-xs text-ink-soft">You can also set this later from the sidebar.</div>}
          </div>
        )}

        {step === 3 && (
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="text-sm font-medium">Choose your agent</div>
              <button
                type="button"
                onClick={() => void reDetect()}
                disabled={reChecking}
                className="text-xs underline text-ink-soft hover:text-ink disabled:opacity-40"
              >
                {reChecking ? "Checking…" : "Re-check"}
              </button>
            </div>
            {/* A CLI runtime the user selected but that isn't on PATH won't
                answer — warn (and link to install) instead of letting them finish
                believing they're set up. */}
            {((choice === "claudeCode" && env && !env.claudeCode) ||
              (choice === "codex" && env && !env.codex)) && (
              <div
                className="rounded-xl border px-3 py-2 text-xs"
                style={{ borderColor: "color-mix(in srgb, var(--ligus-warn) 45%, transparent)", background: "color-mix(in srgb, var(--ligus-warn) 12%, transparent)" }}
              >
                <span style={{ color: "var(--ligus-warn-ink)" }}>
                  {choice === "claudeCode" ? "Claude Code" : "Codex"} isn't on your PATH yet, so
                  @ligus won't answer until it's installed.
                </span>{" "}
                <button
                  type="button"
                  className="underline"
                  onClick={() =>
                    void openExternal(
                      choice === "claudeCode"
                        ? "https://docs.anthropic.com/en/docs/claude-code/overview"
                        : "https://github.com/openai/codex",
                    ).catch(() => {})
                  }
                >
                  Install instructions ↗
                </button>{" "}
                — then <button type="button" className="underline" onClick={() => void reDetect()}>re-check</button>.
              </div>
            )}
            <div className="space-y-1.5">
              <RuntimeOption v="claudeCode" choice={choice} setChoice={setChoice} label="Claude Code" note={env?.claudeCode ? "detected · no API key" : "not found on PATH — install the claude CLI"} />
              {choice === "claudeCode" && (() => {
                const opts = claudeModels.length ? claudeModels : CLAUDE_BASE_MODELS;
                const known = opts.some((o) => o.value === claudeModel);
                const showCustom = claudeCustom || (!known && claudeModel !== "");
                return (
                  <>
                    <select
                      value={showCustom ? "__custom__" : claudeModel}
                      onChange={(e) => {
                        if (e.target.value === "__custom__") {
                          setClaudeCustom(true);
                          setClaudeModel("");
                        } else {
                          setClaudeCustom(false);
                          setClaudeModel(e.target.value);
                        }
                      }}
                      className={field}
                      style={inputStyle}
                      aria-label="Claude Code model"
                    >
                      {opts.map((m) => (
                        <option key={m.value || "default"} value={m.value}>
                          Model: {m.label}
                        </option>
                      ))}
                      <option value="__custom__">Model: Custom…</option>
                    </select>
                    {showCustom && (
                      <input
                        value={claudeModel}
                        onChange={(e) => setClaudeModel(e.target.value)}
                        placeholder="Model alias or id (e.g. fable, claude-fable-5)"
                        className={field}
                        style={inputStyle}
                        aria-label="Custom Claude Code model"
                      />
                    )}
                  </>
                );
              })()}
              <RuntimeOption v="codex" choice={choice} setChoice={setChoice} label="Codex" note={env?.codex ? "detected · no API key (codex login)" : "not found on PATH — install the codex CLI"} />
              <RuntimeOption v="openai" choice={choice} setChoice={setChoice} label="OpenAI-compatible API" note="OpenAI, OpenRouter, or any local OpenAI-style server" />
              <RuntimeOption v="anthropic" choice={choice} setChoice={setChoice} label="Anthropic API key" note={env?.anthropicEnv ? "ANTHROPIC_API_KEY detected" : "claude.ai API key"} />
              {env?.ollama && <RuntimeOption v="ollama" choice={choice} setChoice={setChoice} label="Ollama (local)" note="detected · local models, no key" />}
            </div>

            {choice === "openai" && (
              <div className="space-y-2">
                <input value={apiKey} onChange={(e) => setApiKey(e.target.value)} type="password" placeholder="API key (sk-…, or OpenRouter key)" className={field} style={inputStyle} />
                <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="Base URL (chat completions endpoint)" className={field + " font-mono text-xs"} style={inputStyle} />
                <input value={model} onChange={(e) => setModel(e.target.value)} placeholder="Model (e.g. gpt-4o)" className={field} style={inputStyle} />
              </div>
            )}
            {choice === "anthropic" && (
              <input value={apiKey} onChange={(e) => setApiKey(e.target.value)} type="password" placeholder="Anthropic API key (sk-ant-…)" className={field} style={inputStyle} />
            )}

            {agentNeedsKey && (
              <div className="space-y-1.5">
                <button
                  type="button"
                  onClick={() => void probeAgent()}
                  disabled={agentProbe === "running" || apiKey.trim().length === 0}
                  className="text-xs underline text-ink-soft hover:text-ink disabled:opacity-40"
                >
                  {agentProbe === "running" ? "Testing…" : "Test connection"}
                </button>
                {agentProbe && agentProbe !== "running" && (
                  <div
                    className="text-xs"
                    style={{ color: agentProbe.ok ? "var(--ligus-success-ink)" : "var(--ligus-danger-ink)" }}
                  >
                    {agentProbe.ok
                      ? `✓ Connected (${agentProbe.latency_ms} ms)`
                      : `✕ ${agentProbe.error ?? "Failed to connect"}`}
                  </div>
                )}
              </div>
            )}

            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={letEdit} onChange={(e) => setLetEdit(e.target.checked)} />
              Let agents edit files and run commands (build, test)
            </label>
            <div className="text-xs text-ink-soft">
              {letEdit ? "Agents can create/modify files and run shell commands like cargo/npm (recommended for coding). Web access stays gated — change any of this in Settings → Permissions." : "Read-only — agents can't write files (you can change this later)."}
            </div>
          </div>
        )}

        {step === 4 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Team &amp; sync</div>
            <div className="text-xs text-ink-soft">
              How do you want to sync? You can always change this later in Settings → Team.
            </div>

            <div className="space-y-1.5">
              <TeamModeOption v="local" mode={teamMode} set={pickMode} title="Just me (no sync)" note="Local-only. Nothing leaves this machine." />
              <TeamModeOption v="join" mode={teamMode} set={pickMode} title="Join a team" note="Paste an invite or short code someone shared." />
              <TeamModeOption v="host" mode={teamMode} set={pickMode} title="Sync through a relay" note="The community relay, or one you (or your org) host." />
            </div>

            {teamMode === "join" && (
              <div className="space-y-2 rounded-xl border p-3" style={{ borderColor: "var(--ligus-line)" }}>
                <input
                  value={inviteCode}
                  onChange={(e) => { setInviteCode(e.target.value); setProbe(null); }}
                  placeholder="Invite code (hivews1:…) or short code"
                  className={field + " font-mono text-xs"}
                  style={inputStyle}
                />
                {probe?.status === "unauthorized" && (
                  <input
                    value={accessToken}
                    onChange={(e) => { setAccessToken(e.target.value); setProbe(null); }}
                    placeholder="Relay access token (from your team admin)"
                    className={field + " font-mono text-xs"}
                    style={inputStyle}
                  />
                )}
              </div>
            )}

            {teamMode === "host" && (
              <div className="space-y-2 rounded-xl border p-3" style={{ borderColor: "var(--ligus-line)" }}>
                <RelayPicker
                  choice={relayChoice}
                  onChoice={(c) => { setRelayChoice(c); setProbe(null); }}
                  ownUrl={relayUrl}
                  onOwnUrl={(u) => { setRelayUrl(u); setProbe(null); }}
                  inputClassName={field + " font-mono text-xs"}
                  inputStyle={inputStyle}
                />
                {relayChoice === "own" && (
                  <input
                    value={accessToken}
                    onChange={(e) => { setAccessToken(e.target.value); setProbe(null); }}
                    placeholder="Access token (blank for an open relay)"
                    className={field + " font-mono text-xs"}
                    style={inputStyle}
                  />
                )}
              </div>
            )}

            {teamMode !== "local" && (
              <div className="flex items-center gap-2">
                <button
                  onClick={testConnection}
                  disabled={testing || (teamMode === "join" ? !inviteCode.trim() : !hostRelayUrl)}
                  className="rounded-lg border px-3 py-1.5 text-xs font-medium disabled:opacity-40"
                  style={inputStyle}
                >
                  {testing ? "Testing…" : "Test connection"}
                </button>
                {probe && (
                  <span className="text-xs" style={{ color: connectionOk ? "var(--ligus-success-ink)" : "var(--ligus-accent-warm)" }}>
                    {connectionOk ? "✓ Connected" : probe.detail}
                  </span>
                )}
              </div>
            )}
          </div>
        )}

        {step === 5 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Make it yours</div>
            <div className="text-xs text-ink-soft">Pick a look — change it anytime in Settings → Appearance.</div>
            <div className="grid grid-cols-2 gap-2">
              {THEME_CHOICES.map((t) => {
                const p = resolvePalette(t.name, appearance);
                const active = theme === t.name;
                return (
                  <button
                    key={t.name}
                    type="button"
                    onClick={() => pickTheme(t.name)}
                    className="flex items-center gap-2.5 rounded-xl border px-3 py-2.5 text-left"
                    style={{ borderColor: active ? "var(--ligus-accent-cool)" : "var(--ligus-line)", background: active ? "var(--ligus-mist)" : "transparent" }}
                  >
                    <span
                      className="relative h-8 w-8 shrink-0 overflow-hidden rounded-lg border"
                      style={{ borderColor: "var(--ligus-line)", background: p.canvas }}
                      aria-hidden
                    >
                      <span className="absolute bottom-1 left-1 h-3 w-3 rounded-full" style={{ background: p.accentWarm }} />
                      <span className="absolute right-1 top-1 h-3 w-3 rounded-full" style={{ background: p.accentCool }} />
                    </span>
                    <span className="min-w-0">
                      <span className="block truncate text-sm font-medium">{t.label}</span>
                      <span className="block truncate text-[11px] text-ink-soft">{t.blurb}</span>
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="flex gap-1.5">
              {(["auto", "light", "dark"] as AppearanceMode[]).map((m) => (
                <button
                  key={m}
                  type="button"
                  onClick={() => pickAppearance(m)}
                  className="flex-1 rounded-lg border px-3 py-1.5 text-xs font-medium capitalize"
                  style={{ borderColor: appearance === m ? "var(--ligus-accent-cool)" : "var(--ligus-line)", background: appearance === m ? "var(--ligus-mist)" : "transparent" }}
                >
                  {m}
                </button>
              ))}
            </div>
            <div className="text-xs text-ink-soft">Auto follows your system light/dark setting.</div>
          </div>
        )}

        {error && <p className="mt-3 text-xs" style={{ color: "var(--ligus-warm-ink)" }}>{error}</p>}

        <div className="mt-6 flex items-center justify-between">
          <button className="text-xs text-ink-soft hover:text-ink disabled:opacity-20" disabled={step === 1 || busy} onClick={() => setStep((s) => s - 1)}>
            ← Back
          </button>
          <div className="flex items-center gap-3">
            {step === 2 && (
              <button className="text-xs text-ink-soft hover:text-ink" disabled={busy} onClick={() => setStep(step + 1)}>
                Skip
              </button>
            )}
            <button
              onClick={next}
              disabled={!canNext || busy}
              className="rounded-xl px-5 py-2.5 text-sm font-semibold disabled:opacity-40"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
            >
              {busy ? "…" : step === 5 ? "Finish" : "Next"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function RuntimeOption({
  v,
  choice,
  setChoice,
  label,
  note,
}: {
  v: RuntimeChoice;
  choice: RuntimeChoice;
  setChoice: (c: RuntimeChoice) => void;
  label: string;
  note: string;
}) {
  const active = choice === v;
  return (
    <button
      onClick={() => setChoice(v)}
      className="flex w-full items-center justify-between rounded-xl border px-3 py-2.5 text-left text-sm"
      style={{ borderColor: active ? "var(--ligus-accent-cool)" : "var(--ligus-line)", background: active ? "var(--ligus-mist)" : "transparent" }}
    >
      <span className="font-medium">{label}</span>
      <span className="ml-2 text-xs text-ink-soft">{note}</span>
    </button>
  );
}

function TeamModeOption<T extends string>({
  v,
  mode,
  set,
  title,
  note,
}: {
  v: T;
  mode: T;
  set: (m: T) => void;
  title: string;
  note: string;
}) {
  const active = mode === v;
  return (
    <button
      onClick={() => set(v)}
      className="flex w-full items-center justify-between gap-2 rounded-xl border px-3 py-2 text-left"
      style={{ borderColor: active ? "var(--ligus-accent-cool)" : "var(--ligus-line)", background: active ? "var(--ligus-mist)" : "transparent" }}
    >
      <span className="text-sm font-medium">{title}</span>
      <span className="text-xs text-ink-soft">{note}</span>
    </button>
  );
}
