import { useEffect, useState } from "react";
import {
  setDisplayName,
  setAvatar,
  myProfile,
  setGitEmail,
  setDefaultRuntime,
  updateConnectionSettings,
  getConnectionSettings,
  detectEnvironment,
  setFirstRunPath,
  firstRunWorkspace,
  passkeyRegister,
  recoveryKeyCreate,
  securityStatus,
  type EnvDetectDto,
  type ClaudePermissionMode,
  type SetupOutcome,
} from "@/lib/ipc";
import { LigusBrandIcon } from "@/components/LigusBrand";
import { Avatar } from "@/components/Avatar";
import { InfoTip } from "@/components/ui";
import { LinkThisDevice } from "@/components/LinkThisDevice";
import { AddAgentSheet } from "@/components/setup/AddAgentSheet";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { FIRST_RUN_DOTS, firstRunDot, firstRunNext, firstWorkspaceName, type FirstRunEvent, type FirstRunStep } from "@/lib/firstRun";

// backgroundColor (not the `background` shorthand) so the global `select`
// rule's chevron background-image survives.
const inputStyle = { borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-panel)" };
const field = "w-full rounded-xl border px-3 py-2.5 text-sm outline-none";

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

  // The agent screen: the one-step sheet, then whether agents may edit.
  const [letEdit, setLetEdit] = useState(true); // Accept edits by default

  useEffect(() => {
    void (async () => {
      try {
        const d = await detectEnvironment();
        setEnv(d);
        if (d.gitName) setName((n) => n || d.gitName!);
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

  /// The agent screen is done: the agent was added (or skipped). An agent
  /// that runs on this device also answers untagged messages here (its
  /// runtime becomes this device's default), and the edit switch is saved.
  const finish = (added?: { outcome: SetupOutcome; inputs: Record<string, string> }) =>
    run(async () => {
      const runtimeId = added?.outcome.results.runtimeId;
      if (runtimeId && added?.inputs["runs-on"] === "here") await setDefaultRuntime(runtimeId);
      const c = await getConnectionSettings();
      const permissionMode: ClaudePermissionMode = letEdit ? "runCommands" : "default";
      await updateConnectionSettings({
        relayUrl: c.relayUrl,
        room: c.room,
        workspaceKey: null,
        apiKey: null,
        relayAccessToken: null,
        permissionMode,
      });
      await onComplete();
    });

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
          <div className="space-y-4" data-testid="first-run-agent">
            <div className="text-sm font-medium">Add your first agent</div>
            <AddAgentSheet inline onClose={() => void finish()} onDone={(outcome, inputs) => void finish({ outcome, inputs })} />
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={letEdit} onChange={(e) => setLetEdit(e.target.checked)} />
              Let agents edit files and run commands
              <InfoTip
                label="About editing files"
                text={
                  letEdit
                    ? "Agents can create and change files and run commands like cargo or npm (recommended for coding). Web access stays gated. Change it in Settings → Permissions."
                    : "Read-only: agents can't write files. You can change this later."
                }
              />
            </label>
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
            {step === "agent" && (
              <button className={quiet} disabled={busy} onClick={() => void finish()}>
                Skip for now
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
