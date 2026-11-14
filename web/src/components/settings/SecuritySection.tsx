import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { passkeyRegister, passkeyRemove, recoveryKeyCreate, rotateWorkspaceKey, securityStatus, workspaceRulesSet } from "@/lib/ipc";

/// Passkeys, the recovery key, workspace rules v2 and the workspace key.
/// Admin actions in a rules-v2 workspace need a passkey confirmation; this is
/// where a person registers theirs (at least two, plus a recovery key).
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
  const rules = useMutation({
    mutationFn: () => workspaceRulesSet(2),
    onSuccess: () => {
      refresh();
      toast.success("Rules v2 is on: admin actions now need a passkey.");
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
      <Section title="Passkeys">
        <p className="text-xs text-ink-soft">
          A passkey confirms that you, not just your device, approved an admin action. Any passkey works: Touch ID
          and iCloud Keychain, Windows Hello, Google Password Manager, a phone, or a security key. Register at least
          two.
        </p>
        <ul className="space-y-1 text-sm" data-testid="passkey-list">
          {(st?.passkeys ?? []).map((p) => (
            <li key={p.credentialId} className="flex items-center justify-between gap-2">
              <code className="truncate">
                {p.recovery ? "Recovery key " : ""}
                {p.credentialId.slice(0, 16)}…
              </code>
              <button
                className="shrink-0 text-xs hover:opacity-80"
                style={{ color: "var(--hive-danger-ink)" }}
                disabled={remove.isPending}
                onClick={() => remove.mutate(p.credentialId)}
              >
                Remove
              </button>
            </li>
          ))}
          {passkeys.length === 0 && <li className="text-xs text-ink-soft">No passkeys yet.</li>}
        </ul>
        {passkeys.length === 1 && (
          <p className="text-xs" style={{ color: "var(--hive-warn-ink)" }}>
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
      <Section title="Workspace rules">
        <p className="text-xs text-ink-soft">
          Rules v2 makes membership, roles, device revocation, key rotation, guards, zones, workflows, agent hosts and
          human-gated approvals need a passkey. It needs a passkey and a recovery key first.
        </p>
        <div className="text-sm">Rules version: {st?.rules ?? "…"}</div>
        <Button
          disabled={!st || st.rules >= 2 || passkeys.length === 0 || !hasRecovery || rules.isPending}
          onClick={() => rules.mutate()}
        >
          Turn on rules v2
        </Button>
      </Section>
      <Section title="Workspace key">
        <p className="text-xs text-ink-soft">
          Rotating seals a new key to every current member's devices and bound hosts. Removed members can't read
          anything sent after it.
        </p>
        <div className="text-sm">Key version: {st?.keyVersion ?? "…"}</div>
        <Button disabled={rotate.isPending} onClick={() => rotate.mutate()}>
          Rotate workspace key
        </Button>
        {st?.secretBackend === "file" && (
          <p className="text-xs" style={{ color: "var(--hive-warn-ink)" }}>
            This device keeps Ligus's secrets in owner-only files, not an OS secret store.
          </p>
        )}
      </Section>
    </>
  );
}
