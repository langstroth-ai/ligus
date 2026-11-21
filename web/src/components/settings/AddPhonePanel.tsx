import { useRef, useState } from "react";
import { inspectPhoneCode, certifyPhoneCode, type PhoneCodeDto } from "@/lib/ipc";
import { Button, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// "Add this phone": paste the code the phone app shows (Settings > This
/// phone; copy it there, or scan its QR code with the phone's camera app and
/// paste here via the clipboard), compare the fingerprint with the phone's
/// screen, then confirm with your passkey. The phone becomes one of your
/// devices at the phone tier (it can post, vote and answer as you, never
/// change members, devices or keys), and can then pair with your box.
export function AddPhonePanel({ onDone, onCancel }: { onDone: () => void; onCancel: () => void }) {
  const [code, setCode] = useState("");
  const [preview, setPreview] = useState<PhoneCodeDto | null>(null);
  const [matches, setMatches] = useState(false);
  const [busy, setBusy] = useState(false);
  // The code the textarea holds now: a check that answers after the code
  // changed is dropped, so the sheet never shows another code's phone.
  const latest = useRef("");

  async function check(next: string) {
    latest.current = next;
    setCode(next);
    setPreview(null);
    setMatches(false);
    if (!next.trim()) return;
    try {
      const got = await inspectPhoneCode(next);
      if (latest.current === next) setPreview(got);
    } catch (e) {
      if (latest.current === next && next.trim().startsWith("ligusphone1:")) toast.error(errMsg(e));
    }
  }

  async function certify() {
    if (!preview) return;
    setBusy(true);
    try {
      // Certify only the key whose fingerprint was compared, into the
      // workspace the preview was for (refused if the active one changed).
      await certifyPhoneCode(code, preview.fingerprint, preview.workspaceId);
      toast.success(
        `${preview.label || "Your phone"} is now one of your devices. Give your box a few seconds to sync it, then tap Retry on the phone (or scan a new \`ligus pair\` link if the old one used its five attempts).`,
      );
      onDone();
    } catch (e) {
      toast.error(`Couldn't add the phone: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const blocked = !preview || preview.standing !== "new";
  return (
    <div className="space-y-2 rounded-xl border px-3 py-3" style={fieldStyle} data-testid="add-phone">
      <p className="text-xs text-ink-soft">
        In the phone app, open <em>Settings &gt; This phone</em> and copy its code, then paste it here.
      </p>
      <textarea
        value={code}
        onChange={(e) => void check(e.target.value)}
        placeholder="Paste the phone's code: ligusphone1:…"
        rows={3}
        className="w-full rounded-xl border px-3 py-2 font-mono text-xs"
        style={fieldStyle}
        aria-label="Phone code"
      />
      {preview && (
        <div className="space-y-1.5 text-xs">
          <div>
            <span className="text-ink-soft">Phone</span> <strong>{preview.label || "unnamed"}</strong>
            <span className="text-ink-soft" data-testid="phone-name-hint">
              {" "}
              (the name the phone gives itself; the fingerprint is what proves it's yours)
            </span>
          </div>
          <div>
            <span className="text-ink-soft">Fingerprint</span>{" "}
            <code className="font-bold tracking-wider" data-testid="phone-fingerprint">
              {preview.fingerprint}
            </code>
          </div>
          <div className="truncate font-mono text-[11px] text-ink-soft">device {preview.deviceId}</div>
          {preview.standing === "alreadyMine" && <div className="opacity-70">Already one of your devices.</div>}
          {preview.problem && <div style={{ color: "var(--ligus-danger-ink)" }}>{preview.problem}</div>}
          {preview.standing === "new" && (
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={matches} onChange={(e) => setMatches(e.target.checked)} />
              The phone's screen shows the same fingerprint
            </label>
          )}
        </div>
      )}
      <div className="flex gap-2">
        <Button variant="primary" size="md" disabled={blocked || !matches || busy} onClick={() => void certify()}>
          {busy ? "Waiting for your passkey…" : "Add this phone"}
        </Button>
        <Button size="md" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
