import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, ListRow, Section, fieldStyle } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { toast, errMsg } from "@/components/Toast";
import {
  deviceConfirm,
  passkeyRegister,
  passkeyRemove,
  recoveryKeyCreate,
  rotateWorkspaceKey,
  securityStatus,
  workspaceRulesSet,
} from "@/lib/ipc";
import { confirmDialog } from "@/components/Dialog";

/// Passkeys, the recovery key and the workspace key. Every workspace is on
/// rules v2 from its first event, so admin actions need a passkey
/// confirmation; this is where a person registers theirs (at least two, plus
/// a recovery key).
export function SecuritySection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["security-status"], queryFn: securityStatus });
  const [label, setLabel] = useState("");
  const [words, setWords] = useState<string | null>(null);
  const [registering, setRegistering] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: ["security-status"] });
  const switchRules = useMutation({
    mutationFn: () => workspaceRulesSet(3),
    onSuccess: () => {
      refresh();
      toast.success("This workspace is on rules 3.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const register = useMutation({
    mutationFn: () => passkeyRegister(label.trim() || "Passkey"),
    onSuccess: () => {
      setLabel("");
      setRegistering(false);
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
      <Section
        title="Passkeys"
        action={
          <Button size="sm" onClick={() => setRegistering(true)}>
            Register a passkey
          </Button>
        }
        info="A passkey confirms that you, not just your device, approved an admin action. Any passkey works: Touch ID and iCloud Keychain, Windows Hello, Google Password Manager, a phone, or a security key. Register at least two."
      >
        {status.isError && (
          <p role="alert" className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
            Couldn't read this workspace's passkeys: {errMsg(status.error)}
          </p>
        )}
        {status.isPending && <p className="text-xs text-ink-soft">Loading…</p>}
        <ul className="ligus-rowlist text-sm" data-testid="passkey-list">
          {(st?.passkeys ?? []).map((p) => (
            <ListRow
              key={p.credentialId}
              title={
                <code>
                  {p.recovery ? "Recovery key " : ""}
                  {p.credentialId.slice(0, 16)}…
                </code>
              }
              actionsLabel={`Actions for ${p.recovery ? "recovery key" : "passkey"} ${p.credentialId.slice(0, 16)}`}
              actions={[{ label: "Remove", danger: true, disabled: remove.isPending, onSelect: () => remove.mutate(p.credentialId) }]}
            />
          ))}
          {st && passkeys.length === 0 && <li className="ligus-listrow px-1 text-xs text-ink-soft">No passkeys yet.</li>}
        </ul>
        {passkeys.length === 1 && (
          <p className="text-xs" style={{ color: "var(--ligus-warn-ink)" }}>
            Register another passkey so losing one device doesn't lock you out.
          </p>
        )}
        {registering && (
          <Sheet
            title="Register a passkey"
            side="center"
            width={420}
            onClose={() => setRegistering(false)}
            footer={
              <>
                <Button onClick={() => setRegistering(false)}>Cancel</Button>
                <Button variant="primary" size="md" disabled={register.isPending} onClick={() => register.mutate()}>
                  Add passkey
                </Button>
              </>
            }
          >
            <input
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="MacBook Touch ID"
              aria-label="Passkey name"
              maxLength={64}
              className="w-full rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
          </Sheet>
        )}
      </Section>
      <Section title="Recovery key" info="24 words that stand in for a lost passkey.">
        <p className="text-xs text-ink-soft">Shown once and never stored: write them down and keep them offline.</p>
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
      {st && st.rules < 3 && st.isOwner && (
        <Section
          title="Stricter device rules"
          info="Rules 3: a device counts only once it accepted its approval, duplicate members retire into their person, and model keys are refused on the log. Devices on Ligus 0.3 become read-only here."
        >
          <p className="text-xs text-ink-soft" data-testid="rules3-intro">
            New workspaces start on these rules. Switching this one is confirmed with your passkey and can't be undone.
          </p>
          {(st.rules3Gaps ?? []).length === 0 ? (
            <p className="text-xs text-ink-soft" data-testid="rules3-ready">
              Every device here already counts under them.
            </p>
          ) : (
            <ul className="text-xs" data-testid="rules3-gaps">
              {(st.rules3Gaps ?? []).map((g) => (
                <li key={g}>{g}</li>
              ))}
            </ul>
          )}
          <Button
            disabled={switchRules.isPending}
            onClick={async () => {
              const ok = await confirmDialog("Switch this workspace to rules 3? Devices on Ligus 0.3 become read-only here.", {
                confirmLabel: "Switch",
              });
              if (ok) switchRules.mutate();
            }}
          >
            Switch to rules 3
          </Button>
        </Section>
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
