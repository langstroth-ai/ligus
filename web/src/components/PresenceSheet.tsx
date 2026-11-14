import { useEffect, useState } from "react";
import { Button, Modal } from "@/components/ui";
import { onPresenceDone, onPresenceRequested, presenceCancel, type PresencePromptDto } from "@/lib/ipc";

/// "Confirm with passkey": shown while the system browser runs the WebAuthn
/// ceremony for one event. It names exactly what is being authorized (the
/// event kind and a summary) and the first characters of the digest the
/// passkey signs, so the person can match it with the browser page.
export function PresenceSheet() {
  const [prompt, setPrompt] = useState<PresencePromptDto | null>(null);
  useEffect(() => {
    const subs = [onPresenceRequested(setPrompt), onPresenceDone(() => setPrompt(null))];
    return () => {
      subs.forEach((p) => void p.then((un) => un()).catch(() => {}));
    };
  }, []);
  if (!prompt) return null;
  const cancel = () => {
    void presenceCancel().catch(() => {});
    setPrompt(null);
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
        <div className="flex justify-end">
          <Button onClick={cancel}>Cancel</Button>
        </div>
      </div>
    </Modal>
  );
}
