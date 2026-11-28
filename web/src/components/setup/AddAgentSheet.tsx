// Add an agent: one sheet, one button (workspace model v3, 17.4). The engine
// (`ligus_runtime::setup`, the agent flow) builds the form: what answers
// (this device's coding CLIs and every host's offers, with their state), the
// model from that source's list, and where it runs with its one line. This
// renders it top to bottom and runs it; the CLI's `ligus setup agent` and
// lt's form are the same engine form.
//
// Calm by rule: the fields in order, each explanation behind a ?, one
// primary action. The API key lives in a ref (never React state) and goes
// to the backend once, in `secrets`.

import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  onSetupConfirming,
  setupCancel,
  setupForm,
  setupRun,
  type SetupCheck,
  type SetupConfirming,
  type SetupField,
  type SetupOutcome,
} from "@/lib/ipc";
import { Sheet } from "@/components/kit";
import { Button, ErrorState, LoadingState } from "@/components/ui";
import { errMsg } from "@/components/Toast";
import { applies, Checks, FieldView, InfoTip } from "@/components/setup/SetupWizard";

/// Inputs that change the form (the source's models, where it runs, the line
/// under Runs on).
const DEPENDS = ["source", "runs-on", "endpoint", "name"];
/// Changing an agent: the model and where it runs decide what it asks for.
const DEPENDS_CHANGE = ["agent", "runs-on", "model"];

/// What the sheet changes on an existing agent: its model, or where it runs
/// (the agent's ⋯ menu: Change model…, Runs on…). The same engine form,
/// with `agent` set; only that one field shows.
export type AgentChange = "model" | "runs-on";

export function AddAgentSheet({
  onClose,
  onDone,
  inline = false,
  initialName,
  agent,
  change,
}: {
  onClose: () => void;
  /// After a run that finished (exit 0), with what it wrote and the inputs.
  onDone?: (outcome: SetupOutcome, inputs: Record<string, string>) => void;
  /// Render in place (the first run), without the sheet's frame.
  inline?: boolean;
  initialName?: string;
  /// Change this existing agent instead of adding one.
  agent?: { id: string; name: string };
  change?: AgentChange;
}) {
  const qc = useQueryClient();
  const editing = !!agent;
  const [inputs, setInputs] = useState<Record<string, string>>(
    agent ? { agent: agent.id } : initialName ? { name: initialName } : {},
  );
  const secrets = useRef<Record<string, string>>({});
  const [secretNonce, setSecretNonce] = useState(0);
  const [running, setRunning] = useState(false);
  const [confirming, setConfirming] = useState<SetupConfirming | null>(null);
  const [outcome, setOutcome] = useState<SetupOutcome | null>(null);
  const [runError, setRunError] = useState<string | null>(null);

  // Text is debounced before it asks the engine again (the name only moves
  // the line under Runs on and the button's label).
  const depNow = (editing ? DEPENDS_CHANGE : DEPENDS).map((k) => inputs[k] ?? "").join("|");
  const [depKey, setDepKey] = useState(depNow);
  useEffect(() => {
    const t = setTimeout(() => setDepKey(depNow), 250);
    return () => clearTimeout(t);
  }, [depNow]);
  const form = useQuery({
    queryKey: ["setup-form", "agent", editing ? "change" : "add", depKey],
    queryFn: () => setupForm("agent", sendable(inputs)),
    placeholderData: (prev) => prev,
  });

  // Defaults in, once per field (the best source, where it runs, a model).
  useEffect(() => {
    if (!form.data) return;
    setInputs((cur) => {
      const next = { ...cur };
      for (const s of form.data.steps)
        for (const f of s.fields ?? [])
          if (f.default !== undefined && f.type !== "secret" && next[f.id] === undefined) next[f.id] = f.default;
      return next;
    });
  }, [form.data]);

  useEffect(() => {
    let off: (() => void) | undefined;
    let live = true;
    void onSetupConfirming((e) => setConfirming(e))
      .then((u) => {
        if (live) off = u;
        else u();
      })
      .catch(() => {});
    return () => {
      live = false;
      off?.();
      secrets.current = {};
    };
  }, []);

  const fields: SetupField[] = (form.data?.steps ?? []).flatMap((s) => s.fields ?? []);
  // Changing an agent shows only the field it changes; the agent itself
  // rides along unseen.
  const shown = fields.filter((f) => applies(f, inputs) && (!editing || f.id === (change ?? "model")));
  const missing = shown.filter((f) => f.required && f.type !== "secret" && !(inputs[f.id] ?? "").trim());
  const confirmations = form.data?.confirmations ?? 0;

  function set(id: string, v: string) {
    setOutcome(null);
    setInputs((cur) => {
      const next = { ...cur, [id]: v };
      // A new source has its own models and hosts: let the engine pick again.
      if (id === "source") {
        delete next.model;
        delete next["runs-on"];
        delete next.endpoint;
      }
      if (id === "runs-on") delete next.model;
      return next;
    });
  }

  function sendable(cur: Record<string, string>): Record<string, string> {
    const out: Record<string, string> = {};
    const known = form.data ? (form.data.steps ?? []).flatMap((s) => s.fields ?? []) : null;
    for (const [k, v] of Object.entries(cur)) {
      if (v === undefined || v === "") continue;
      const f = known?.find((x) => x.id === k);
      if (known && (!f || f.type === "secret" || !applies(f, cur))) continue;
      out[k] = v;
    }
    return out;
  }

  async function run() {
    setRunning(true);
    setRunError(null);
    setOutcome(null);
    setConfirming(null);
    const sent = sendable(inputs);
    const secretValues = Object.fromEntries(
      Object.entries(secrets.current).filter(([k, v]) => v && shown.some((f) => f.id === k && f.type === "secret")),
    );
    try {
      const out = await setupRun("agent", sent, secretValues);
      setOutcome(out);
      if (out.exitCode === 0) {
        void qc.invalidateQueries();
        onDone?.(out, sent);
      }
    } catch (e) {
      setRunError(errMsg(e));
    } finally {
      secrets.current = {};
      setSecretNonce((n) => n + 1);
      setRunning(false);
      setConfirming(null);
    }
  }

  function close() {
    if (running) void setupCancel("agent");
    secrets.current = {};
    onClose();
  }

  const label = form.data?.action ?? (editing ? "Save" : "Add the agent");
  const title = !agent ? "Add an agent" : change === "runs-on" ? `Where @${agent.name} runs` : `Change model for @${agent.name}`;
  const ok = outcome?.exitCode === 0;

  let body;
  if (form.isError) body = <ErrorState text={`Couldn't load this: ${errMsg(form.error)}`} onRetry={() => void form.refetch()} />;
  else if (!form.data) body = <LoadingState rows={2} label="Looking for what can answer" />;
  else
    body = (
      <div className="space-y-4" key={secretNonce}>
        {form.data.preflight.some((c) => c.status === "fail") && <Checks checks={form.data.preflight.filter((c) => c.status !== "ok")} />}
        {shown.map((f) => {
          // Runs on lists only where it can run; the engine keeps the rest
          // to say why a typed host can't.
          const field = f.id === "runs-on" ? { ...f, options: (f.options ?? []).filter((o) => !o.disabled) } : f;
          return (
            <div key={f.id}>
              <FieldView
                field={field}
                value={inputs[f.id] ?? ""}
                onChange={(v) => set(f.id, v)}
                onSecret={(v) => {
                  secrets.current[f.id] = v;
                }}
              />
              {f.note && (
                <p className="mt-1 text-[12px]" style={{ color: "var(--ligus-ink-soft)" }} data-testid={`note-${f.id}`}>
                  {f.note}
                </p>
              )}
            </div>
          );
        })}
        {editing && confirmations === 1 && !running && !ok && (
          <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }} data-testid="change-agent-passkey">
            Asks for your passkey once.
          </p>
        )}
        {confirmations > 1 && !running && !ok && (
          <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }} data-testid="add-agent-confirmations">
            Asks for your passkey twice: once for the model source, once for the agent.
          </p>
        )}
        {running && (
          <p className="text-[13px]" role="status">
            {confirming && confirming.of > 1 ? `Confirm ${confirming.step} of ${confirming.of}: ${confirming.what}` : confirming ? `Confirm with your passkey: ${confirming.what}` : "Adding…"}
          </p>
        )}
        {runError && <ErrorState text={runError} />}
        {outcome && <OutcomeView outcome={outcome} editing={editing} />}
      </div>
    );

  const button = ok ? (
    <Button variant="primary" onClick={close}>
      Done
    </Button>
  ) : (
    <Button
      variant="primary"
      disabled={!form.data || form.data.blocked || running || missing.length > 0}
      title={missing.length ? `Needs ${missing.map((f) => f.label.toLowerCase()).join(", ")}` : undefined}
      onClick={() => void run()}
    >
      {label}
    </Button>
  );

  if (inline)
    return (
      <div className="space-y-4" data-testid="add-agent-inline">
        {body}
        <div className="flex justify-end">{button}</div>
      </div>
    );

  return (
    <Sheet
      title={title}
      titleAddon={!editing && form.data?.flow.why ? <InfoTip text={form.data.flow.why} label="About agents" /> : undefined}
      side="center"
      width={460}
      onClose={close}
      footer={
        <>
          {!ok && <Button onClick={close}>Cancel</Button>}
          {button}
        </>
      }
    >
      {body}
    </Sheet>
  );
}

/// What the run said: done in one line, or what failed and how to fix it.
function OutcomeView({ outcome, editing = false }: { outcome: SetupOutcome; editing?: boolean }) {
  if (outcome.exitCode === 0) {
    const agent = outcome.checks.find((c) => c.id === (editing ? "agent.change" : "agent.agent"));
    return (
      <p className="text-[13px] font-medium" style={{ color: "var(--ligus-success-ink)" }}>
        {agent?.detail ?? "Ready."}
      </p>
    );
  }
  const bad: SetupCheck[] = outcome.checks.filter((c) => c.status === "fail" || (outcome.exitCode === 4 && c.status === "warn"));
  return (
    <div className="space-y-1.5" data-testid="add-agent-failed">
      {bad.map((c) => (
        <div key={c.id} className="text-[12.5px]">
          <span className="font-medium">{c.detail}</span>
          {c.fix && (
            <div className="mt-0.5" style={{ color: "var(--ligus-ink-soft)" }}>
              {c.fix}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
