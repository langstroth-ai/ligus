// The guided setup wizard: one flow of the engine (`ligus_runtime::setup`)
// as a sheet, one step per screen. The engine decides the steps, the fields,
// the checks and their fix lines; this only renders them.
//
// Calm by rule: a title and the fields on each screen, the explanation
// behind an (i), the passed checks folded away, a failure's fix right under
// it. Secret fields live in a ref (never React state, drafts or storage),
// go to the backend once, and are cleared after a run and on close. The
// steps only a person can do (admitting a box, pairing a phone, creating an
// invite) open their own screens.

import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  pickWorkspaceFolder,
  setupCancel,
  setupForm,
  setupRun,
  workspaceInvite,
  type SetupCheck,
  type SetupField,
  type SetupFlowId,
  type SetupOutcome,
  type SetupStep,
} from "@/lib/ipc";
import { Sheet } from "@/components/kit";
import { Button, ErrorState, LoadingState, Switch, fieldStyle } from "@/components/ui";
import { tip } from "@/components/Tooltip";
import { IconAlertTriangle, IconCheck, IconInfo } from "@/lib/icons";
import { openAdmitBox, requestAddPhone } from "@/lib/nav";
import { errMsg } from "@/components/Toast";

/// Fields that change other fields' options (the models of a harness).
const DEPENDS = ["harness", "kind", "endpoint"];

function applies(f: SetupField, inputs: Record<string, string>): boolean {
  if (!f.when) return true;
  const [k, vals] = f.when;
  const v = inputs[k] ?? "";
  return vals.includes(v);
}

export function SetupWizard({ flow, workspaceId, onClose }: { flow: SetupFlowId; workspaceId?: string; onClose: () => void }) {
  const qc = useQueryClient();
  const [inputs, setInputs] = useState<Record<string, string>>({});
  // Masked values: a ref, so they are never in React state or a re-render.
  const secrets = useRef<Record<string, string>>({});
  const [secretNonce, setSecretNonce] = useState(0);
  const [screen, setScreen] = useState(0);
  const [outcome, setOutcome] = useState<SetupOutcome | null>(null);
  const [running, setRunning] = useState(false);
  const [runError, setRunError] = useState<string | null>(null);

  const depKey = DEPENDS.map((k) => inputs[k] ?? "").join("|");
  const form = useQuery({
    queryKey: ["setup-form", flow, depKey],
    queryFn: () => setupForm(flow, inputs),
    placeholderData: (prev) => prev,
  });

  // Defaults in, once per field.
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

  // Secrets never outlive the sheet.
  useEffect(
    () => () => {
      secrets.current = {};
    },
    [],
  );

  const steps: SetupStep[] = form.data?.steps ?? [];
  const inputSteps = useMemo(
    () => steps.filter((s) => s.kind === "input" && (s.fields ?? []).some((f) => applies(f, inputs))),
    [steps, inputs],
  );
  const preflightIssues = (form.data?.preflight ?? []).filter((c) => c.status !== "ok");
  // A flow with nothing to fill in (a box, a phone, an invite) still opens
  // on its checks, so the person sees what happens before it does.
  const hasCheck = preflightIssues.length > 0 || !!form.data?.blocked || (!!form.data && inputSteps.length === 0);
  const screens: string[] = [...(hasCheck ? ["check"] : []), ...inputSteps.map((s) => s.id), "done"];
  const labels = [...(hasCheck ? ["Check"] : []), ...inputSteps.map((s) => s.title), steps.find((s) => s.kind === "verify")?.title ?? "Done"];
  const current = screens[Math.min(screen, screens.length - 1)];
  const step = inputSteps.find((s) => s.id === current);
  const lastInput = screen === screens.length - 2;

  const missing = (step?.fields ?? []).filter((f) => applies(f, inputs) && f.required && f.type !== "secret" && !(inputs[f.id] ?? "").trim());

  function set(id: string, v: string) {
    setInputs((cur) => ({ ...cur, [id]: v }));
  }

  async function run() {
    setScreen(screens.length - 1);
    setRunning(true);
    setRunError(null);
    setOutcome(null);
    const fields = steps.flatMap((s) => s.fields ?? []);
    const sent: Record<string, string> = {};
    for (const f of fields) {
      if (f.type === "secret" || !applies(f, inputs)) continue;
      const v = inputs[f.id];
      if (v !== undefined && v !== "") sent[f.id] = v;
    }
    const secretValues = Object.fromEntries(
      Object.entries(secrets.current).filter(([k, v]) => v && fields.some((f) => f.id === k && applies(f, inputs))),
    );
    try {
      const out = await setupRun(flow, sent, secretValues);
      setOutcome(out);
      if (out.exitCode === 0) void qc.invalidateQueries();
    } catch (e) {
      setRunError(errMsg(e));
    } finally {
      secrets.current = {};
      setSecretNonce((n) => n + 1);
      setRunning(false);
    }
  }

  function close() {
    if (running) void setupCancel(flow);
    secrets.current = {};
    onClose();
  }

  const title = form.data?.flow.title ?? "Set up";
  const why = form.data?.flow.why;

  let body: ReactNode;
  if (form.isError) body = <ErrorState text={`Couldn't load this setup: ${errMsg(form.error)}`} onRetry={() => void form.refetch()} />;
  else if (!form.data) body = <LoadingState rows={2} label="Loading setup" />;
  else if (current === "check")
    body = (
      <div className="space-y-3">
        <Checks checks={form.data.preflight} />
        {!form.data.blocked &&
          steps
            .filter((s) => s.kind === "person")
            .map((s) => (
              <p key={s.id} className="text-[13px]">
                {s.title}
                {s.where && <span style={{ color: "var(--ligus-ink-soft)" }}> · {s.where}</span>}
              </p>
            ))}
      </div>
    );
  else if (step)
    body = (
      <div className="space-y-4" key={`${step.id}-${secretNonce}`}>
        {(step.fields ?? [])
          .filter((f) => applies(f, inputs))
          .map((f) => (
            <FieldView
              key={f.id}
              field={f}
              value={inputs[f.id] ?? ""}
              onChange={(v) => set(f.id, v)}
              onSecret={(v) => {
                secrets.current[f.id] = v;
              }}
            />
          ))}
      </div>
    );
  else body = <Result flow={flow} workspaceId={workspaceId} running={running} error={runError} outcome={outcome} onClose={close} />;

  const footer = (() => {
    if (!form.data) return <Button onClick={close}>Close</Button>;
    if (current === "done") {
      if (running) return <Button onClick={() => void setupCancel(flow)}>Cancel</Button>;
      const ok = outcome?.exitCode === 0;
      return (
        <>
          {!ok && inputSteps.length > 0 && <Button onClick={() => setScreen(screens.length - 2)}>Back</Button>}
          {!ok && (
            <Button variant={outcome?.exitCode === 4 ? "ghost" : "primary"} onClick={() => void run()}>
              {outcome?.exitCode === 4 ? "Check again" : "Try again"}
            </Button>
          )}
          {ok && (
            <Button variant="primary" onClick={close}>
              Done
            </Button>
          )}
        </>
      );
    }
    if (current === "check" && form.data.blocked) return <Button onClick={close}>Close</Button>;
    return (
      <>
        {screen > 0 && <Button onClick={() => setScreen(screen - 1)}>Back</Button>}
        <Button
          variant="primary"
          disabled={missing.length > 0}
          title={missing.length ? `Needs ${missing.map((f) => f.label.toLowerCase()).join(", ")}` : undefined}
          onClick={() => (lastInput || (current === "check" && inputSteps.length === 0) ? void run() : setScreen(screen + 1))}
        >
          {lastInput || (current === "check" && inputSteps.length === 0) ? (flow === "box" || flow === "phone" || flow === "invite" ? "Continue" : "Set up") : "Next"}
        </Button>
      </>
    );
  })();

  return (
    <Sheet
      title={title}
      description={why ? <InfoTip text={why} label="What this sets up" /> : undefined}
      steps={labels}
      step={Math.min(screen, screens.length - 1)}
      onClose={close}
      footer={footer}
    >
      {body}
    </Sheet>
  );
}

/// The explanation behind an (i): a small button with the text as its tip.
function InfoTip({ text, label }: { text: string; label: string }) {
  return (
    <span className="inline-flex items-center gap-1 text-[12px]">
      <button type="button" aria-label={`${label}: ${text}`} {...tip(text)} className="ligus-press inline-flex h-5 w-5 items-center justify-center rounded-full" style={{ color: "var(--ligus-ink-soft)" }}>
        <IconInfo size={13} />
      </button>
    </span>
  );
}

function FieldView({ field, value, onChange, onSecret }: { field: SetupField; value: string; onChange: (v: string) => void; onSecret: (v: string) => void }) {
  const id = `setup-${field.id}`;
  const head = (
    <div className="mb-1.5 flex items-center gap-1">
      <label htmlFor={id} className="text-[12px] font-semibold" style={{ color: "var(--ligus-ink-soft)" }}>
        {field.label}
        {!field.required && field.type !== "toggle" && <span className="font-normal"> (optional)</span>}
      </label>
      {field.help && <InfoTip text={field.help} label={field.label} />}
    </div>
  );
  const input = "w-full rounded-lg border px-2.5 py-1.5 text-[13px] outline-none";
  switch (field.type) {
    case "choice":
      return (
        <div>
          {head}
          <div role="radiogroup" aria-label={field.label} id={id} className="space-y-1">
            {(field.options ?? []).map((o) => {
              const on = value === o.value;
              return (
                <button
                  key={o.value}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  disabled={o.disabled}
                  onClick={() => onChange(o.value)}
                  className="ligus-press flex w-full items-center gap-2 rounded-lg border px-2.5 py-1.5 text-left text-[13px] disabled:opacity-50"
                  style={{
                    borderColor: on ? "color-mix(in srgb, var(--ligus-accent-cool) 45%, transparent)" : "var(--ligus-line)",
                    background: on ? "color-mix(in srgb, var(--ligus-accent-cool) 12%, transparent)" : "transparent",
                  }}
                >
                  <span className="min-w-0 flex-1 truncate">{o.label}</span>
                  {o.hint && (
                    <span className="shrink-0 text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                      {o.hint}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </div>
      );
    case "multi": {
      const picked = value.split(",").map((s) => s.trim()).filter(Boolean);
      const toggle = (v: string) => {
        if (v === "all") return onChange("all");
        const rest = picked.filter((p) => p !== "all");
        const next = rest.includes(v) ? rest.filter((p) => p !== v) : [...rest, v];
        onChange(next.length ? next.join(",") : "all");
      };
      return (
        <div>
          {head}
          <div role="group" aria-label={field.label} id={id} className="space-y-1">
            {(field.options ?? []).map((o) => (
              <label key={o.value} className="flex items-center gap-2 text-[13px]">
                <input type="checkbox" checked={picked.includes(o.value) || (o.value === "all" && picked.length === 0)} onChange={() => toggle(o.value)} />
                {o.label}
              </label>
            ))}
          </div>
        </div>
      );
    }
    case "suggest":
      return (
        <div>
          {head}
          <input id={id} list={`${id}-list`} value={value} onChange={(e) => onChange(e.target.value)} className={input} style={fieldStyle} placeholder="Default" />
          <datalist id={`${id}-list`}>
            {(field.options ?? []).map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </datalist>
        </div>
      );
    case "secret":
      return (
        <div>
          {head}
          <input
            id={id}
            type="password"
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => onSecret(e.target.value)}
            className={input}
            style={fieldStyle}
            placeholder="Kept in this device's secret store"
          />
        </div>
      );
    case "path":
      return (
        <div>
          {head}
          <div className="flex gap-2">
            <input id={id} value={value} onChange={(e) => onChange(e.target.value)} className={input} style={fieldStyle} spellCheck={false} />
            <Button
              onClick={() =>
                void pickWorkspaceFolder().then((p) => {
                  if (p) onChange(p);
                })
              }
            >
              Choose…
            </Button>
          </div>
        </div>
      );
    case "toggle":
      return (
        <div className="flex items-center justify-between gap-3">
          {head}
          <Switch on={value === "true"} onChange={(v) => onChange(v ? "true" : "")} label={field.label} />
        </div>
      );
    default:
      return (
        <div>
          {head}
          <input id={id} value={value} onChange={(e) => onChange(e.target.value)} className={input} style={fieldStyle} spellCheck={false} />
        </div>
      );
  }
}

/// Checks: what needs attention in full (with its fix), what passed folded.
function Checks({ checks }: { checks: SetupCheck[] }) {
  // An explanation the person needs (an invite's role) stays in view.
  const keep = (c: SetupCheck) => c.status === "warn" || c.status === "fail" || c.id.endsWith(".role");
  const issues = checks.filter(keep);
  const rest = checks.filter((c) => !keep(c));
  return (
    <div className="space-y-2">
      {issues.map((c) => (
        <CheckRow key={c.id} c={c} />
      ))}
      {rest.length > 0 && (
        <details className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }} open={issues.length === 0}>
          <summary className="cursor-pointer select-none">{issues.length === 0 ? "All checks passed" : `${rest.length} more`}</summary>
          <div className="mt-1.5 space-y-1">
            {rest.map((c) => (
              <CheckRow key={c.id} c={c} />
            ))}
          </div>
        </details>
      )}
    </div>
  );
}

function CheckRow({ c }: { c: SetupCheck }) {
  const bad = c.status === "fail";
  const warn = c.status === "warn";
  return (
    <div className="flex items-start gap-2 text-[12.5px]" data-check={c.id} data-status={c.status}>
      <span
        aria-hidden
        className="mt-0.5 shrink-0"
        style={{ color: bad ? "var(--ligus-danger-ink)" : warn ? "var(--ligus-warn-ink)" : "var(--ligus-success-ink)" }}
      >
        {bad || warn ? <IconAlertTriangle size={13} /> : <IconCheck size={13} />}
      </span>
      <div className="min-w-0 flex-1">
        <span className="sr-only">{c.status}: </span>
        <span className="font-medium">{c.title}</span>
        <span style={{ color: "var(--ligus-ink-soft)" }}> · {c.detail}</span>
        {c.fix && c.status !== "ok" && (
          <div className="mt-0.5 text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {c.fix}
          </div>
        )}
      </div>
    </div>
  );
}

/// The run's result, and the hand-off when a person must act.
function Result({
  flow,
  workspaceId,
  running,
  error,
  outcome,
  onClose,
}: {
  flow: SetupFlowId;
  workspaceId?: string;
  running: boolean;
  error: string | null;
  outcome: SetupOutcome | null;
  onClose: () => void;
}) {
  if (running) return <LoadingState rows={2} label="Setting up" />;
  if (error) return <ErrorState text={error} />;
  if (!outcome) return null;
  // A person step: its warning is the hand-off itself, said once above the
  // button that opens it.
  const person = outcome.exitCode === 4 ? [...outcome.checks].reverse().find((c) => c.status === "warn") : undefined;
  const shown = person ? outcome.checks.filter((c) => c !== person) : outcome.checks;
  return (
    <div className="space-y-4">
      {outcome.exitCode === 0 && (
        <p className="text-[13px] font-medium" style={{ color: "var(--ligus-success-ink)" }}>
          {outcome.dryRun ? "Checked. Nothing was changed." : "Ready."}
        </p>
      )}
      {shown.length > 0 && <Checks checks={shown} />}
      {person && (
        <div className="space-y-2.5">
          <p className="text-[13px]">{person.detail}</p>
          <Handoff flow={flow} workspaceId={workspaceId} onClose={onClose} />
        </div>
      )}
    </div>
  );
}

/// The step only a person can do, in its own screen.
function Handoff({ flow, workspaceId, onClose }: { flow: SetupFlowId; workspaceId?: string; onClose: () => void }) {
  const [code, setCode] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // An invite code is a bearer secret: shown here once, dropped on close.
  useEffect(() => () => setCode(null), []);
  if (flow === "box")
    return (
      <Button
        variant="primary"
        onClick={() => {
          onClose();
          openAdmitBox();
        }}
      >
        Open Admit a box
      </Button>
    );
  if (flow === "phone")
    return (
      <Button
        variant="primary"
        onClick={() => {
          onClose();
          requestAddPhone();
        }}
      >
        Open Add a phone
      </Button>
    );
  if (flow === "invite") {
    if (code)
      return (
        <div className="space-y-2">
          <label htmlFor="setup-invite-code" className="text-[12px] font-semibold" style={{ color: "var(--ligus-ink-soft)" }}>
            Invite code
          </label>
          <textarea id="setup-invite-code" readOnly value={code} rows={3} className="w-full rounded-lg border px-2.5 py-1.5 font-mono text-[11.5px]" style={fieldStyle} />
          <div className="flex items-center gap-2">
            <Button onClick={() => void navigator.clipboard?.writeText(code).catch(() => {})}>Copy</Button>
            <span className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
              Single use. Send it only to the person it is for.
            </span>
          </div>
        </div>
      );
    return (
      <div className="space-y-2">
        <Button
          variant="primary"
          disabled={!workspaceId}
          onClick={() => {
            setErr(null);
            void workspaceInvite(workspaceId!)
              .then(setCode)
              .catch((e) => setErr(errMsg(e)));
          }}
        >
          Create invite
        </Button>
        {err && <ErrorState text={err} />}
      </div>
    );
  }
  return null;
}
