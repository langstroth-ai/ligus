import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import {
  deviceConfirm,
  passkeyRegister,
  passkeyRemove,
  recoveryKeyCreate,
  rotateWorkspaceKey,
  securityStatus,
} from "@/lib/ipc";

/// Passkeys, the recovery key and the workspace key. Every workspace is on
/// rules v2 from its first event, so admin actions need a passkey
/// confirmation; this is where a person registers theirs (at least two, plus
/// a recovery key).
export function SecuritySection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["security-status"], queryFn: securityStatus });
  const [label, setLabel] = useState("");
  const [words, setWords] = useState<string | null>(null);
  const refresh = () => qc.invalidateQueries({ queryKey: ["security-status"] });
  const register = useMutation({
    mutationFn: () => passkeyRegister(label.trim() || "Passkey"),
    onSuccess: () => {
      setLabel("");
      refresh();
      toast.success("Passkey registered.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const recovery = useMutation({
    mutationFn: recoveryKeyCreate,
    onSuccess: (w) => {
      setWords(w);
      refresh();
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const remove = useMutation({
    mutationFn: passkeyRemove,
    onSuccess: () => {
      refresh();
      toast.success("Removed.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const confirmDevice = useMutation({
    mutationFn: deviceConfirm,
    onSuccess: () => {
      refresh();
      toast.success("This device is confirmed.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const rotate = useMutation({
    mutationFn: rotateWorkspaceKey,
    onSuccess: (r) => {
      refresh();
      toast.success(`Workspace key rotated (sealed to ${r.recipients} devices).`);
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  const st = status.data;
  const passkeys = st?.passkeys.filter((p) => !p.recovery) ?? [];
  const hasRecovery = st?.passkeys.some((p) => p.recovery) ?? false;

  return (
    <>
      {st && !st.deviceConfirmed && (
        <Section title="This device">
          <p className="text-xs" style={{ color: "var(--ligus-warn-ink)" }}>
            This device isn't confirmed for this workspace yet. A new device of an account with a passkey counts
            only after that passkey confirms it; until then other devices don't accept its changes.
          </p>
          <Button variant="primary" size="md" disabled={confirmDevice.isPending} onClick={() => confirmDevice.mutate()}>
            Confirm this device
          </Button>
        </Section>
      )}
      <Section title="Passkeys">
        <p className="text-xs text-ink-soft">
          A passkey confirms that you, not just your device, approved an admin action. Any passkey works: Touch ID
          and iCloud Keychain, Windows Hello, Google Password Manager, a phone, or a security key. Register at least
          two.
        </p>
        {status.isError && (
          <p role="alert" className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
            Couldn't read this workspace's passkeys: {errMsg(status.error)}
          </p>
        )}
        {status.isPending && <p className="text-xs text-ink-soft">Loading…</p>}
        <ul className="space-y-1 text-sm" data-testid="passkey-list">
          {(st?.passkeys ?? []).map((p) => (
            <li key={p.credentialId} className="flex items-center justify-between gap-2">
              <code className="truncate">
                {p.recovery ? "Recovery key " : ""}
                {p.credentialId.slice(0, 16)}…
              </code>
              <button
                className="shrink-0 text-xs hover:opacity-80"
                style={{ color: "var(--ligus-danger-ink)" }}
                disabled={remove.isPending}
                onClick={() => remove.mutate(p.credentialId)}
              >
                Remove
              </button>
            </li>
          ))}
          {st && passkeys.length === 0 && <li className="text-xs text-ink-soft">No passkeys yet.</li>}
        </ul>
        {passkeys.length === 1 && (
          <p className="text-xs" style={{ color: "var(--ligus-warn-ink)" }}>
            Register another passkey so losing one device doesn't lock you out.
          </p>
        )}
        <div className="flex gap-2">
          <input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="MacBook Touch ID"
            maxLength={64}
            className="flex-1 rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
          />
          <Button variant="primary" size="md" disabled={register.isPending} onClick={() => register.mutate()}>
            Add passkey
          </Button>
        </div>
      </Section>
      <Section title="Recovery key">
        <p className="text-xs text-ink-soft">
          24 words that stand in for a lost passkey. They're shown once and never stored: write them down and keep
          them offline.
        </p>
        {words ? (
          <div className="space-y-2 rounded-xl border px-3 py-2.5" style={fieldStyle}>
            <code className="block whitespace-pre-wrap text-sm" data-testid="recovery-words">
              {words}
            </code>
            <Button onClick={() => setWords(null)}>I wrote them down</Button>
          </div>
        ) : (
          <Button disabled={passkeys.length === 0 || recovery.isPending} onClick={() => recovery.mutate()}>
            {hasRecovery ? "Create a new recovery key" : "Create recovery key"}
          </Button>
        )}
      </Section>
      {st && (passkeys.length === 0 || !hasRecovery) && (
        <p className="text-xs" style={{ color: "var(--ligus-warn-ink)" }} data-testid="finish-securing">
          Finish securing this workspace: membership, roles, devices and keys need your passkey, and a recovery key
          keeps a lost passkey from locking you out.
        </p>
      )}
      <Section title="Workspace key">
        <p className="text-xs text-ink-soft">
          Rotating seals a new key to every current member's devices, including the serve and worker boxes enrolled
          for them. Removed members can't read messages or attachments sent after it.
        </p>
        <div className="text-sm">Key version: {st?.keyVersion ?? "…"}</div>
        <Button disabled={rotate.isPending} onClick={() => rotate.mutate()}>
          Rotate workspace key
        </Button>
        {st?.secretBackend === "file" && (
          <p className="text-xs" style={{ color: "var(--ligus-warn-ink)" }}>
            This device keeps Ligus's secrets in owner-only files, not an OS secret store.
          </p>
        )}
        {st?.secretBackend === "unavailable" && (
          <p className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
            The Ligus keychain isn't available, so Ligus can't save secrets on this device. Ligus won't fall back to
            plain files on its own: fix the keychain, or start Ligus with LIGUS_SECRET_BACKEND=file to keep them in
            owner-only files.
          </p>
        )}
      </Section>
    </>
  );
}
