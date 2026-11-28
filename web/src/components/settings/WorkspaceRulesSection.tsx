import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, FormDisclosure, Section } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { confirmDialog } from "@/components/Dialog";
import { setupRun, workspaceRules4Status, workspaceRulesUpgrade, type PinOfferDto } from "@/lib/ipc";

/// Settings > Workspace rules (workspace model v3, 15.2). Below rules 4 the
/// Owner sees what upgrading changes and the readiness list (devices on an
/// older Ligus by name and build: they go read-only here), and one button:
/// one passkey, never back. On rules 4, agents that still name a runtime can
/// be pinned to what it runs, so their model is theirs to change.
export function WorkspaceRulesSection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["rules4-status"], queryFn: workspaceRules4Status });
  const [showChanges, setShowChanges] = useState(false);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["rules4-status"] });
    void qc.invalidateQueries({ queryKey: ["security-status"] });
  };
  const upgrade = useMutation({
    mutationFn: workspaceRulesUpgrade,
    onSuccess: () => {
      refresh();
      toast.success("This workspace is on rules 4.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const pin = useMutation({
    mutationFn: async (p: PinOfferDto) => {
      const out = await setupRun("agent", { agent: p.agentId }, {});
      if (out.exitCode !== 0) {
        const bad = out.checks.find((c) => c.status === "fail" || c.status === "warn");
        throw new Error(bad ? bad.detail : "it didn't finish");
      }
      return p;
    },
    onSuccess: (p) => {
      refresh();
      void qc.invalidateQueries({ queryKey: ["agents"] });
      toast.success(`@${p.name} now carries its own model choice.`);
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  if (status.isError)
    return (
      <Section title="Workspace rules">
        <p role="alert" className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
          Couldn't read this workspace's rules: {errMsg(status.error)}
        </p>
      </Section>
    );
  const st = status.data;
  if (!st) return <p className="text-xs text-ink-soft">Loading…</p>;

  if (st.rules < 4) {
    if (!st.isOwner)
      return (
        <Section title="Workspace rules">
          <p className="text-xs text-ink-soft" data-testid="rules4-not-owner">
            This workspace is on rules {st.rules}. Its Owner can upgrade it.
          </p>
        </Section>
      );
    const readOnly = st.oldDevices.map((d) => d.name);
    return (
      <Section
        title="Upgrade workspace rules"
        info="Rules decide what every device accepts here. Rules 4 gives each agent its own model choice and keeps model server addresses on the hosts that run them. New workspaces start on it."
      >
        <p className="text-xs text-ink-soft" data-testid="rules4-intro">
          Upgrading takes your passkey once and can't be undone.
        </p>
        {st.readiness.length === 0 ? (
          <p className="text-xs text-ink-soft" data-testid="rules4-ready">
            Every device here is ready.
          </p>
        ) : (
          <ul className="space-y-1 text-xs" data-testid="rules4-readiness">
            {st.readiness.map((l) => (
              <li key={l}>{l}</li>
            ))}
          </ul>
        )}
        <FormDisclosure label="What changes" open={showChanges} onToggle={() => setShowChanges((v) => !v)}>
          <ul className="space-y-1 px-2.5 text-xs text-ink-soft" data-testid="rules4-changes">
            {st.changes.map((c) => (
              <li key={c}>{c}</li>
            ))}
          </ul>
        </FormDisclosure>
        <Button
          variant="primary"
          size="md"
          disabled={upgrade.isPending}
          onClick={async () => {
            const devices = readOnly.length ? ` ${readOnly.join(", ")} ${readOnly.length === 1 ? "goes" : "go"} read-only here until updated.` : "";
            const ok = await confirmDialog(`Upgrade this workspace to rules 4?${devices} This can't be undone.`, {
              title: "Upgrade workspace rules",
              confirmLabel: "Upgrade",
            });
            if (ok) upgrade.mutate();
          }}
        >
          Upgrade workspace rules
        </Button>
      </Section>
    );
  }

  return (
    <>
      <Section title="Workspace rules">
        <p className="text-xs text-ink-soft" data-testid="rules4-on">
          This workspace is on rules 4.
        </p>
      </Section>
      {st.pins.length > 0 && (
        <Section
          title="Pin agents to what they run"
          info="These agents still name a runtime. Pinning one makes it carry its own model choice, so changing its model later needs no passkey. Each pin asks for your passkey once."
        >
          <ul className="ligus-rowlist text-sm" data-testid="rules4-pins">
            {st.pins.map((p) => (
              <li key={p.agentId} className="ligus-listrow flex items-center justify-between gap-2 px-1">
                <span className="min-w-0 truncate text-[13px]">
                  {p.label}
                  {!p.mine && <span className="text-ink-soft"> · someone else's</span>}
                </span>
                <Button size="sm" disabled={pin.isPending} onClick={() => pin.mutate(p)}>
                  Pin
                </Button>
              </li>
            ))}
          </ul>
        </Section>
      )}
    </>
  );
}
