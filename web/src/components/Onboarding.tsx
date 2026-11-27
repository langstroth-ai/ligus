import { useEffect, useState } from "react";
import {
  setDisplayName,
  setAvatar,
  myProfile,
  setGitEmail,
  addRuntime,
  setClaudeCodeModel,
  setDefaultModel,
  listClaudeCodeModels,
  updateConnectionSettings,
  getConnectionSettings,
  detectEnvironment,
  openExternal,
  probeProvider,
  listOllamaModels,
  setFirstRunPath,
  firstRunWorkspace,
  passkeyRegister,
  recoveryKeyCreate,
  securityStatus,
  type RuntimeTestResult,
  type EnvDetectDto,
  type ClaudeModelOption,
  type ClaudePermissionMode,
} from "@/lib/ipc";
import { LigusBrandIcon } from "@/components/LigusBrand";
import { Avatar } from "@/components/Avatar";
import { InfoTip } from "@/components/ui";
import { LinkThisDevice } from "@/components/LinkThisDevice";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { FIRST_RUN_DOTS, firstRunDot, firstRunNext, firstWorkspaceName, type FirstRunEvent, type FirstRunStep } from "@/lib/firstRun";

type RuntimeChoice = "claudeCode" | "codex" | "openai" | "anthropic" | "ollama";

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

/// The first run. It asks one question first, "Is this your first device
/// with Ligus?" (`lib/firstRun` has the screens and their order):
///
/// - **I already use Ligus**: link this device (`LinkThisDevice`). No name,
///   no workspace.
/// - **Yes, start here**: You (name, picture), then Secure (the person's
///   first workspace is made, then a passkey and the recovery key; the only
///   way out before it's done is Finish later, which asks first), then the
///   agent. Folder, relay and team are asked when they first mean something;
///   the theme lives in Settings → Appearance.
export function Onboarding({ onComplete }: { onComplete: () => void | Promise<void> }) {
  const [step, setStep] = useState<FirstRunStep>("fork");
  const go = (event: FirstRunEvent) => setStep((s) => firstRunNext(s, event));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [env, setEnv] = useState<EnvDetectDto | null>(null);

  // You
  const [name, setName] = useState("");
  const [picture, setPicture] = useState<string | null>(null);
  const [safetyCode, setSafetyCode] = useState<string | null>(null);

  // Secure: the passkey, then the recovery key's words (shown once).
  const [hasPasskey, setHasPasskey] = useState(false);
  // A linked device can't make the person's passkey: when the account has
  // none yet, the first run says where to add it before it ends.
  const [linkedWithoutPasskey, setLinkedWithoutPasskey] = useState(false);
  const [words, setWords] = useState<string | null>(null);

  // The agent screen: runtime + access
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
  // Ollama gets its own model: the OpenAI field's "gpt-4o" default must never
  // reach an Ollama runtime. Prefilled from the server's installed models.
  const [ollamaModel, setOllamaModel] = useState("");
  const [ollamaInstalled, setOllamaInstalled] = useState<string[]>([]);
  useEffect(() => {
    if (choice !== "ollama") return;
    let live = true;
    void listOllamaModels({ endpoint: "http://localhost:11434" })
      .then((r) => {
        if (!live) return;
        const names = r.models.map((m) => m.name);
        setOllamaInstalled(names);
        setOllamaModel((cur) => cur || names[0] || "");
      })
      .catch(() => {
        /* no list: the plain field still works */
      });
    return () => {
      live = false;
    };
  }, [choice]);
  // Any change to the agent choice/creds invalidates a prior probe result.
  useEffect(() => {
    setAgentProbe(null);
    setAgentAckFail(false);
  }, [choice, apiKey, model, baseUrl]);
  const [claudeModel, setClaudeModel] = useState(""); // "" = CLI default
  const [claudeModels, setClaudeModels] = useState<ClaudeModelOption[]>([]);
  const [claudeCustom, setClaudeCustom] = useState(false);
  const [letEdit, setLetEdit] = useState(true); // Accept edits by default

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

  async function run(f: () => Promise<void>) {
    setError(null);
    setBusy(true);
    try {
      await f();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  /// The question's answer, recorded on this data dir (the invite guard
  /// reads it), then its first screen.
  const choose = (path: "first" | "link") =>
    run(async () => {
      await setFirstRunPath(path);
      go(path === "first" ? "chooseFirst" : "chooseLink");
    });

  /// You → Secure: save the name and picture, and make the person's first
  /// workspace (a first run started again reuses the one it made).
  const saveYou = () =>
    run(async () => {
      await setDisplayName(name.trim());
      if (env?.gitEmail) await setGitEmail(env.gitEmail);
      if (picture) await setAvatar(picture);
      await firstRunWorkspace(firstWorkspaceName(name));
      // A first run started again may find its passkey already made.
      try {
        const st = await securityStatus();
        setHasPasskey(st.passkeys.some((p) => !p.recovery));
        if (st.passkeys.some((p) => p.recovery)) {
          go("named");
          go("secured");
          return;
        }
      } catch {
        /* the Secure screen still works from the start */
      }
      go("named");
    });

  /// Secure's one button: the passkey, then the recovery key (each is one
  /// confirmation in the system's passkey dialog).
  const secure = () =>
    run(async () => {
      if (!hasPasskey) {
        await passkeyRegister("Passkey");
        setHasPasskey(true);
      }
      setWords(await recoveryKeyCreate());
    });

  /// Linked as one of the person's devices: the workspace and profile are
  /// theirs already, so the first run ends here, after Secure only if the
  /// account has no passkey yet.
  const linked = async () => {
    let passkey = true;
    try {
      passkey = (await securityStatus()).passkeys.some((p) => !p.recovery);
    } catch {
      /* the status isn't there yet: nothing to ask */
    }
    if (passkey) await onComplete();
    else setLinkedWithoutPasskey(true);
  };

  const finish = () =>
    run(async () => {
      // Guarantee a responding agent: for an API provider, verify the key
      // actually answers before finishing. A first failure warns and holds;
      // a second Finish (agentAckFail) goes on anyway so a transient probe
      // failure can't trap the user.
      if (agentNeedsKey && !(typeof agentProbe === "object" && agentProbe?.ok) && !agentAckFail) {
        const ok = await probeAgent();
        if (!ok) {
          setAgentAckFail(true);
          setError(
            "Couldn't reach that provider with this key — @ligus won't answer until it works. Fix the key, or press Finish again to set it up anyway.",
          );
          return;
        }
      }
      await applyRuntime();
      await onComplete();
    });

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
      await addRuntime("ollama-local", "Ollama (local)", "ollama", "local", "http://localhost:11434", ollamaModel.trim() || "llama3.1", true, false);
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

  const dot = firstRunDot(step);
  const primary = (label: string, onClick: () => void, disabled = false) => (
    <button
      onClick={onClick}
      disabled={disabled || busy}
      className="rounded-xl px-5 py-2.5 text-sm font-semibold disabled:opacity-40"
      style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
    >
      {busy ? "…" : label}
    </button>
  );
  const quiet = "text-xs text-ink-soft hover:text-ink disabled:opacity-20";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-6" style={{ background: "var(--ligus-canvas)" }}>
      <div className="w-full max-w-md rounded-3xl border p-8" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}>
        <div className="mb-4 flex items-center gap-3">
          <LigusBrandIcon size={40} />
          <h1 className="ligus-display">Welcome to Ligus</h1>
        </div>
        {dot > 0 && (
          <div className="mb-5 flex gap-1.5" data-testid="first-run-dots">
            {Array.from({ length: FIRST_RUN_DOTS }, (_, i) => i + 1).map((n) => (
              <div key={n} className="h-1 flex-1 rounded-full" style={{ background: n <= dot ? "var(--ligus-accent-cool)" : "var(--ligus-line)" }} />
            ))}
          </div>
        )}

        {step === "fork" && (
          <div className="space-y-3" data-testid="first-run-fork">
            <div className="flex items-center gap-1.5">
              <div className="text-sm font-medium">Is this your first device with Ligus?</div>
              <InfoTip
                label="this question"
                text="Ligus keeps one you across your computers and phone. If you already use it somewhere, link this device to that account instead of starting again: starting again here would make a second you, with its own name and keys."
              />
            </div>
            <button
              onClick={() => void choose("first")}
              disabled={busy}
              className="w-full rounded-xl px-5 py-2.5 text-sm font-semibold disabled:opacity-40"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
            >
              Yes, start here
            </button>
            <button onClick={() => void choose("link")} disabled={busy} className={"w-full py-1.5 " + quiet}>
              I already use Ligus on another device
            </button>
          </div>
        )}

        {step === "link" && !linkedWithoutPasskey && <LinkThisDevice onBack={() => go("back")} onLinked={linked} />}

        {step === "link" && linkedWithoutPasskey && (
          <div className="space-y-3" data-testid="first-run-linked-secure">
            <div className="text-sm font-medium">One thing left: a passkey</div>
            <p className="text-xs text-ink-soft">
              Your account has no passkey yet, so changes to who's in a workspace wait for one. Add it on your first device
              (Settings → Passkeys & keys); this computer uses it from then on.
            </p>
            {primary("Done", () => void run(async () => onComplete()))}
          </div>
        )}

        {step === "you" && (
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
            <div className="flex items-center gap-1.5 text-xs text-ink-soft">
              <span>Shown to people you work with. Change it any time.</span>
              <InfoTip
                label="your account on this device"
                text={`Ligus made an account key on this device; no provider signs you in. Your safety code, which people can compare with the code they see for you: ${safetyCode ?? "…"}`}
              />
            </div>
          </div>
        )}

        {step === "secure" && (
          <div className="space-y-3" data-testid="first-run-secure">
            <div className="flex items-center gap-1.5">
              <div className="text-sm font-medium">Secure your workspace</div>
              <InfoTip
                label="passkeys and the recovery key"
                text="Your passkey confirms changes to who's in a workspace and what agents may do. Ligus never sends it anywhere. The recovery key is 24 words that stand in for a lost passkey: without it, someone who loses every passkey can't make those changes again."
              />
            </div>
            {words ? (
              <>
                <p className="text-xs text-ink-soft">Write down your recovery key and keep it somewhere safe. It is shown once.</p>
                <code className="block whitespace-pre-wrap rounded-lg border p-2 text-xs" style={inputStyle} data-testid="first-run-recovery-words">
                  {words}
                </code>
              </>
            ) : (
              <p className="text-xs text-ink-soft">
                {hasPasskey ? "Your passkey is ready. Now make your recovery key." : "Create a passkey, then a recovery key in case you lose it."}
              </p>
            )}
          </div>
        )}

        {step === "finishLater" && (
          <div className="space-y-3" data-testid="first-run-finish-later">
            <p className="text-sm font-semibold">Finish later?</p>
            <p className="text-xs text-ink-soft">
              Until your workspace has your passkey and recovery key, inviting people and other changes that need your
              passkey stay locked. Finish it any time in Settings → Passkeys & keys.
            </p>
          </div>
        )}

        {step === "agent" && (
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
            {choice === "ollama" && (
              <>
                <input
                  value={ollamaModel}
                  onChange={(e) => setOllamaModel(e.target.value)}
                  list="onboarding-ollama-models"
                  placeholder="Model (e.g. llama3.1)"
                  aria-label="Ollama model"
                  className={field}
                  style={inputStyle}
                />
                <datalist id="onboarding-ollama-models">
                  {ollamaInstalled.map((n) => (
                    <option key={n} value={n} />
                  ))}
                </datalist>
              </>
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


        {error && <p className="mt-3 text-xs" style={{ color: "var(--ligus-warm-ink)" }}>{error}</p>}

        {step !== "fork" && step !== "link" && (
          <div className="mt-6 flex items-center justify-between">
            <span>
              {step === "you" && (
                <button className={quiet} disabled={busy} onClick={() => go("back")}>
                  ← Back
                </button>
              )}
              {step === "secure" && !words && (
                <button className={quiet} disabled={busy} onClick={() => go("askFinishLater")}>
                  Finish later
                </button>
              )}
              {step === "finishLater" && (
                <button className={quiet} onClick={() => go("finishLater")}>
                  Finish later
                </button>
              )}
            </span>
            {step === "you" && primary("Next", () => void saveYou(), name.trim().length === 0)}
            {step === "secure" &&
              (words
                ? primary("I wrote them down", () => go("secured"))
                : primary(hasPasskey ? "Create a recovery key" : "Create a passkey", () => void secure()))}
            {step === "finishLater" && primary("Keep going", () => go("keepGoing"))}
            {step === "agent" &&
              primary(
                "Finish",
                () => void finish(),
                !(choice === "claudeCode" || choice === "codex" || choice === "ollama" || apiKey.trim().length > 0),
              )}
          </div>
        )}
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
