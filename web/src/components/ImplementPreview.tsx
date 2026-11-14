import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Badge, Button, Modal } from "@/components/ui";
import { getImplementPlan, type ImplementPreviewDto } from "@/lib/ipc";
import { errMsg } from "@/components/Toast";

/// The Implement preview sheet (Track D4): exactly what will run, before it
/// runs. The text and argv come from the same backend function the runner
/// uses, so what is shown is what is sent.
export function ImplementPreview({
  sessionId,
  proposalId,
  title,
  onConfirm,
  onClose,
}: {
  sessionId: string;
  proposalId: string;
  title: string;
  onConfirm: (targetAgentId: string | null) => void;
  onClose: () => void;
}) {
  const [target, setTarget] = useState<string | null>(null);
  const plan = useQuery({
    queryKey: ["implement-plan", sessionId, proposalId, target],
    queryFn: () => getImplementPlan(sessionId, proposalId, target),
  });
  return (
    <Modal
      onClose={onClose}
      panelClassName="flex max-h-[88vh] w-[min(96vw,720px)] flex-col overflow-hidden rounded-2xl border"
      panelStyle={{ background: "var(--hive-panel)", borderColor: "var(--hive-line)", boxShadow: "var(--hive-shadow-pop)" }}
    >
      <div className="border-b px-4 py-3" style={{ borderColor: "var(--hive-line)" }}>
        <div className="text-xs text-ink-soft">Implement</div>
        <div className="font-semibold">{title}</div>
      </div>
      <div className="flex-1 overflow-y-auto px-4 py-3 text-sm" data-testid="implement-preview">
        {plan.isPending && <div className="text-ink-soft">Working out what will run…</div>}
        {plan.isError && <div style={{ color: "var(--hive-danger-ink)" }}>Couldn't build the plan: {errMsg(plan.error)}</div>}
        {plan.data && <PlanBody plan={plan.data} target={target} setTarget={setTarget} />}
      </div>
      <div className="flex justify-end gap-2 border-t px-4 py-3" style={{ borderColor: "var(--hive-line)" }}>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="primary" disabled={!plan.data} onClick={() => onConfirm(target ?? plan.data?.targetAgentId ?? null)}>
          {plan.data?.kind === "argv" ? "Run this command" : "Implement"}
        </Button>
      </div>
    </Modal>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex gap-3 py-1">
      <div className="w-32 shrink-0 text-xs text-ink-soft">{label}</div>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

export function PlanBody({
  plan,
  target,
  setTarget,
}: {
  plan: ImplementPreviewDto;
  target: string | null;
  setTarget: (t: string | null) => void;
}) {
  return (
    <>
      {plan.kind === "argv" && (
        <Row label="Runs exactly">
          <pre className="whitespace-pre-wrap break-all rounded-lg px-2 py-1.5 font-mono text-xs" style={{ background: "var(--hive-overlay)" }} data-testid="plan-argv">
            {(plan.argv ?? []).map((a) => (/[\s"'$`\\]/.test(a) ? JSON.stringify(a) : a)).join(" ")}
          </pre>
          <div className="mt-1 text-xs text-ink-soft">
            In the chat's worktree{plan.cwd ? ` (${plan.cwd})` : ""}, sandboxed, with no network. No model is involved.
          </div>
        </Row>
      )}
      {plan.kind === "instruction" && (
        <Row label="Sends">
          <pre className="whitespace-pre-wrap rounded-lg px-2 py-1.5 font-mono text-xs" style={{ background: "var(--hive-overlay)" }} data-testid="plan-instruction">
            {plan.instruction}
          </pre>
        </Row>
      )}
      {plan.kind === "applyDiff" && <Row label="Does">Applies the proposal's diff to the chat's branch.</Row>}
      {plan.kind === "addWorkflow" && <Row label="Does">Adds the workflow definition.</Row>}
      {plan.legacyCommand && (
        <div className="my-1">
          <Badge tone="warn">No exact command was filed: an agent interprets this instruction.</Badge>
        </div>
      )}
      {plan.kind !== "applyDiff" && plan.kind !== "addWorkflow" && (
        <Row label={plan.kind === "argv" ? "For" : "Agent"}>
          <select
            aria-label="Target agent"
            className="rounded-md border px-1.5 py-0.5 text-sm"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
            value={target ?? plan.targetAgentId ?? ""}
            onChange={(e) => setTarget(e.target.value || null)}
          >
            <option value="">@hive (primary)</option>
            {plan.targets.map(([id, name]) => (
              <option key={id} value={id}>
                {name}
              </option>
            ))}
          </select>
        </Row>
      )}
      <Row label="Runtime">
        {plan.runtime}
        {plan.driver ? ` · ${plan.driver === "acp" ? "ACP" : "legacy adapter"}` : ""}
      </Row>
      {plan.permissionTier && <Row label="Permission tier">{plan.permissionTier}</Row>}
      <Row label="Sandbox">{plan.executor}</Row>
      <Row label="Network">{plan.egress.length ? plan.egress.join(", ") : "none"}</Row>
      <div className="mt-2 flex flex-wrap gap-1">
        {plan.humanGate && <Badge tone="warn">Human gate applies</Badge>}
        {plan.presenceRequired && <Badge tone="info">Presence (passkey) required</Badge>}
      </div>
    </>
  );
}
