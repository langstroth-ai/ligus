import { useEffect, useState } from "react";
import { Button, Modal } from "@/components/ui";
import { onPresenceDone, onPresenceRequested, presenceCancel, presenceUseRecovery, type PresencePromptDto } from "@/lib/ipc";

/// "Confirm with passkey": shown while the system browser runs the WebAuthn
/// ceremony for one event. It names exactly what is being authorized (the
/// event kind and a summary) and the first characters of the digest the
/// passkey signs, so the person can match it with the browser page. A person
/// who lost every passkey confirms with their recovery key's 24 words instead
/// (signed on this device; the words are never stored).
export function PresenceSheet() {
  const [prompt, setPrompt] = useState<PresencePromptDto | null>(null);
  const [recovery, setRecovery] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const done = () => {
      setPrompt(null);
      setRecovery(null);
      setError(null);
    };
    const subs = [onPresenceRequested(setPrompt), onPresenceDone(done)];
    return () => {
      subs.forEach((p) => void p.then((un) => un()).catch(() => {}));
    };
  }, []);
  if (!prompt) return null;
  const cancel = () => {
    void presenceCancel().catch(() => {});
    setPrompt(null);
    setRecovery(null);
    setError(null);
  };
  const useRecovery = () => {
    const words = (recovery ?? "").trim();
    setRecovery("");
    presenceUseRecovery(words).catch((e) => setError(String(e)));
  };
  return (
    <Modal onClose={cancel} panelClassName="w-[420px] max-w-full rounded-2xl border p-5 space-y-3">
      <div role="dialog" aria-label="Confirm with passkey" className="space-y-3">
        <h2 className="text-base font-semibold">{prompt.op === "create" ? "Create a passkey" : "Confirm with passkey"}</h2>
        <p className="text-sm">{prompt.summary}</p>
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs text-ink-soft">
          <dt>Event</dt>
          <dd>
            <code>{prompt.kind}</code>
          </dd>
          <dt>Digest</dt>
          <dd>
            <code data-testid="presence-digest">{prompt.digestShort}</code>
          </dd>
        </dl>
        <p className="text-xs text-ink-soft">
          Your browser opened ligus.ai to ask for your passkey. Check the digest there matches, then use Touch ID,
          Windows Hello, your phone or a security key.
        </p>
        {prompt.op === "get" && recovery !== null && (
          <div className="space-y-2">
            <textarea
              aria-label="Recovery key words"
              className="w-full rounded border p-2 text-xs"
              rows={3}
              autoComplete="off"
              spellCheck={false}
              value={recovery}
              onChange={(e) => setRecovery(e.target.value)}
            />
            <Button onClick={useRecovery}>Confirm with recovery key</Button>
          </div>
        )}
        {error && <p className="text-xs text-red-600">{error}</p>}
        <div className="flex justify-between">
          {prompt.op === "get" && recovery === null ? (
            <Button onClick={() => setRecovery("")}>Use recovery key</Button>
          ) : (
            <span />
          )}
          <Button onClick={cancel}>Cancel</Button>
        </div>
      </div>
    </Modal>
  );
}
