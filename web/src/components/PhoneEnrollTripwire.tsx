import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button, Modal } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { onPhoneEnrollTripwire, revokeMyDevice, type PhoneEnrollTripwireDto } from "@/lib/ipc";
import { spellFingerprint } from "@/components/settings/AddPhonePanel";

/// The add-a-phone tripwire (design §8.3), wherever the person is in the
/// app: after a phone was added by QR, the desktop keeps listening until the
/// code expires, and another phone answering the same code means someone
/// else may have seen it. If the phone that was added isn't the one in the
/// person's hand, they revoke it here (confirmed with the passkey, as every
/// revocation is).
export function PhoneEnrollTripwire() {
  const [alerts, setAlerts] = useState<PhoneEnrollTripwireDto[]>([]);
  const [busy, setBusy] = useState(false);
  const qc = useQueryClient();
  useEffect(() => {
    const sub = onPhoneEnrollTripwire((a) =>
      setAlerts((xs) =>
        xs.some((x) => x.rid === a.rid && x.other.deviceId === a.other.deviceId) ? xs : [...xs, a],
      ),
    );
    return () => {
      void sub.then((un) => un()).catch(() => {});
    };
  }, []);
  const a = alerts[0];
  if (!a) return null;
  const dismiss = () => setAlerts((xs) => xs.slice(1));
  const added = a.added.label || "the phone";

  async function revoke() {
    setBusy(true);
    try {
      await revokeMyDevice(a.added.deviceId);
      toast.success(`${added} was revoked. Add your phone again with a new code.`);
      qc.invalidateQueries({ queryKey: ["my-devices"] });
      setAlerts((xs) => xs.filter((x) => x.added.deviceId !== a.added.deviceId));
    } catch (e) {
      toast.error(`Couldn't revoke: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal onClose={dismiss} panelClassName="w-[440px] max-w-full rounded-2xl border p-5 space-y-3">
      <div role="alertdialog" aria-label="Another phone answered this code" className="space-y-3" data-testid="phone-tripwire">
        <h2 className="ligus-title">Another phone answered this code</h2>
        <p className="text-sm">
          Another phone answered this code after you added <strong>{added}</strong> (fingerprint{" "}
          <code aria-label={spellFingerprint(a.added.fingerprint)}>{a.added.fingerprint}</code>). If the phone you added
          isn't the one in your hand, revoke it now.
        </p>
        <p className="text-xs text-ink-soft">
          The other phone calls itself {a.other.label || "nothing"}, fingerprint{" "}
          <code aria-label={spellFingerprint(a.other.fingerprint)}>{a.other.fingerprint}</code>. Compare with the
          fingerprint your phone shows.
        </p>
        <div className="flex justify-end gap-2">
          <Button size="md" onClick={dismiss} disabled={busy}>
            It's mine
          </Button>
          <Button variant="danger" size="md" onClick={() => void revoke()} disabled={busy}>
            {busy ? "Waiting for your passkey…" : `Revoke ${added}`}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
