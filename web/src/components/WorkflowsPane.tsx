import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listAgents,
  listWorkflows,
  listWorkflowRuns,
  removeWorkflow,
  setWorkflowEnabled,
  workflowChangeRequest,
  sendMessage,
  startWorkflowRun,
  cancelWorkflowRun,
  resumeWorkflowRun,
  handOverWorkflowRun,
  listWorkspaceHosts,
  voteProposal,
  onWorkflowRun,
  onChatStream,
  onWorkspaceSynced,
  type WorkflowDefinitionDto,
  type WorkflowNodeDto,
  type WorkflowRunDto,
  type WorkflowNodeRunDto,
  type WorkspaceHostDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { confirmDialog } from "@/components/Dialog";
import { Badge, Button, Section, ErrorState, LinkButton } from "@/components/ui";
import { MoreMenu } from "@/components/MoreMenu";
import { PANE_DOCS } from "@/lib/docs";
import { IconChevronRight, IconLock } from "@/lib/icons";
import { RailFrame, Stack, EmptyHint, panelStyle, fieldStyle } from "@/components/RightRail";
import { RunLeaseLine } from "@/components/guards/RunLeaseLine";

/// Visual tone for a node/run status chip. Exported for tests.
export function statusTone(status: string): { color: string; background: string } {
  switch (status) {
    case "running":
      return { color: "var(--ligus-accent-ink)", background: "color-mix(in srgb, var(--ligus-accent-cool) 12%, transparent)" };
    case "awaitingApproval":
    case "awaitingGate":
      return { color: "var(--ligus-warn-ink)", background: "color-mix(in srgb, var(--ligus-accent-warm) 14%, transparent)" };
    case "succeeded":
    case "completed":
      return { color: "var(--ligus-success-ink)", background: "color-mix(in srgb, var(--ligus-success) 14%, transparent)" };
    case "failed":
    case "rejected":
    case "halted":
      return { color: "var(--ligus-danger-ink)", background: "color-mix(in srgb, var(--ligus-danger) 14%, transparent)" };
    default: // pending, skipped, canceled
      return { color: "var(--ligus-ink)", background: "var(--ligus-overlay)" };
  }
}

/// Human label for a status ("awaitingApproval" → "awaiting approval").
export function statusLabel(status: string): string {
  return status.replace(/([A-Z])/g, " $1").toLowerCase();
}

/// Stages in an order a person reads top to bottom: dependencies first,
/// ties in stored order. A malformed graph (it can't be: definitions are
/// validated) falls back to stored order. Exported for tests.
export function readingOrder(nodes: WorkflowNodeDto[]): WorkflowNodeDto[] {
  const placed = new Set<string>();
  const out: WorkflowNodeDto[] = [];
  const ids = new Set(nodes.map((n) => n.id));
  let progress = true;
  while (out.length < nodes.length && progress) {
    progress = false;
    for (const n of nodes) {
      if (placed.has(n.id)) continue;
      if (n.dependsOn.every((d) => placed.has(d) || !ids.has(d))) {
        placed.add(n.id);
        out.push(n);
        progress = true;
      }
    }
  }
  return out.length === nodes.length ? out : nodes;
}

/// A read-only rendering of a definition's stages: who runs each one, what
/// it waits for, its prompt, and how a gate decides. Shared by the Workflows
/// pane and workflow proposals in the Review pane.
export function WorkflowStages({
  nodes,
  agentName,
}: {
  nodes: WorkflowNodeDto[];
  agentName: (agentId: string) => string | undefined;
}) {
  return (
    <ol className="mt-2 flex flex-col gap-1.5">
      {readingOrder(nodes).map((n, i) => (
        <li key={n.id} className="rounded-lg border px-2.5 py-1.5 text-xs" style={{ borderColor: "var(--ligus-line)" }}>
          <div className="flex flex-wrap items-baseline gap-x-1.5">
            <span className="text-ink-soft">{i + 1}.</span>
            <span className="font-medium">
              {n.kind === "gate" ? <span aria-label="approval gate" className="mr-1 inline-flex align-[-2px]"><IconLock size={11} /></span> : null}
              {n.name}
            </span>
            <span className="text-ink-soft">
              {n.kind === "gate"
                ? `approval gate · ${n.requiredApprovals ?? 1} approval${(n.requiredApprovals ?? 1) === 1 ? "" : "s"}`
                : n.agentId
                  ? `@${agentName(n.agentId) ?? "agent not in this chat"}`
                  : "primary runtime"}
            </span>
            {n.dependsOn.length > 0 && <span className="text-ink-soft">after {n.dependsOn.join(", ")}</span>}
          </div>
          {n.kind === "gate" ? (
            <div className="mt-0.5 text-ink-soft">
              {n.gateTitle}
              {" · "}
              {n.onReject === "routeTo" && n.rejectTarget ? `reject retries from ${n.rejectTarget}` : "reject halts the run"}
            </div>
          ) : (
            n.promptTemplate && (
              <div className="mt-0.5 max-h-32 overflow-auto whitespace-pre-wrap break-words text-ink-soft">
                {n.promptTemplate}
              </div>
            )
          )}
        </li>
      ))}
    </ol>
  );
}

export function WorkflowsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const workflows = useQuery({
    queryKey: ["workflows", sessionId],
    queryFn: () => listWorkflows(sessionId),
  });
  const runs = useQuery({
    queryKey: ["workflow-runs", sessionId],
    queryFn: () => listWorkflowRuns(sessionId),
  });
  const agents = useQuery({
    queryKey: ["agents", sessionId],
    queryFn: () => listAgents(sessionId),
  });
  // Online devices, for the "hand over to" picker on a run whose driver
  // stopped beating.
  const hosts = useQuery({ queryKey: ["workspace-hosts"], queryFn: listWorkspaceHosts });
  const agentName = (id: string) => agents.data?.find((a) => a.id === id)?.name;
  const deletes = usePendingDeletes(sessionId);

  // Live run updates: the engine emits workflow://run on every persisted
  // transition; gates also create proposals and stages post chat messages.
  useEffect(() => {
    const unlisten = onWorkflowRun((e) => {
      if (e.sessionId !== sessionId) return;
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
      qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [sessionId, qc]);

  // An approved workflow proposal adds a definition (here or on a teammate's
  // device, arriving via sync), so refresh after turns and syncs.
  useEffect(() => {
    const unlistenStream = onChatStream((e) => {
      if (e.sessionId === sessionId && e.phase !== "delta") {
        qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
      }
    });
    const unlistenSync = onWorkspaceSynced(() => {
      qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
    });
    return () => {
      unlistenStream.then((fn) => fn());
      unlistenSync.then((fn) => fn());
    };
  }, [sessionId, qc]);

  return (
    <RailFrame
      title="Workflows"
      about="Multi-stage agent pipelines with approval gates. Agents write them; you approve, run and pause them."
      docs={PANE_DOCS.workflows}
    >
      <Section
        title="Definitions"
        info="Ask an agent in this chat to set one up, e.g. “build a workflow that drafts, reviews, and waits for my approval”. It arrives in the Review pane; approve it and it shows up here."
      >
        <Stack>
          {(workflows.data ?? []).map((wf) =>
            deletes.pending.has(wf.id) ? (
              <DeletedRow key={wf.id} name={wf.name} onUndo={() => deletes.undo(wf.id)} />
            ) : (
              <DefinitionCard
                key={wf.id}
                sessionId={sessionId}
                wf={wf}
                agentName={agentName}
                onDelete={() => deletes.schedule(wf.id)}
              />
            ),
          )}
          {workflows.isError ? (
            <ErrorState text="Couldn't load workflows." onRetry={() => void workflows.refetch()} />
          ) : (
            (workflows.data ?? []).length === 0 && (
              <EmptyHint text="No workflows yet." />
            )
          )}
        </Stack>
      </Section>

      <Section title="Runs">
        <Stack>
          {(runs.data ?? []).map((run) => (
            <RunCard key={run.id} sessionId={sessionId} run={run} hosts={hosts.data ?? []} />
          ))}
          {runs.isError ? (
            <ErrorState text="Couldn't load runs." onRetry={() => void runs.refetch()} />
          ) : (
            (runs.data ?? []).length === 0 && <EmptyHint text="No runs yet." />
          )}
        </Stack>
      </Section>
    </RailFrame>
  );
}

/// How long a deleted workflow can be brought back before the delete is
/// written (it syncs to everyone, and there's no way to re-add a definition
/// short of a new proposal, so undo means "not sent yet").
export const UNDO_MS = 8_000;

/// Workflow deletes waiting out their undo window. Leaving the pane (or the
/// chat) sends any still waiting: the person confirmed them.
function usePendingDeletes(sessionId: string) {
  const qc = useQueryClient();
  // Timers live in a ref (state updaters may run twice); `pending` mirrors
  // their keys for rendering.
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [pending, setPending] = useState<ReadonlySet<string>>(() => new Set());
  const sync = () => setPending(new Set(timers.current.keys()));

  const send = useCallback(
    (id: string) =>
      removeWorkflow(sessionId, id)
        .catch((e) => toast.error(`Couldn't delete the workflow: ${errMsg(e)}`))
        .finally(() => qc.invalidateQueries({ queryKey: ["workflows", sessionId] })),
    [sessionId, qc],
  );

  const schedule = (id: string) => {
    if (timers.current.has(id)) return;
    timers.current.set(
      id,
      setTimeout(() => {
        timers.current.delete(id);
        sync();
        void send(id);
      }, UNDO_MS),
    );
    sync();
  };

  const undo = (id: string) => {
    const t = timers.current.get(id);
    if (t === undefined) return;
    clearTimeout(t);
    timers.current.delete(id);
    sync();
  };

  useEffect(() => {
    const live = timers.current;
    return () => {
      for (const [id, t] of live) {
        clearTimeout(t);
        void send(id);
      }
      live.clear();
    };
  }, [send]);

  return { pending, schedule, undo };
}

/// What deleting a workflow loses, said before it happens. Runs keep a frozen
/// copy of their definition, so runs already going finish and the history
/// stays.
export function workflowDeleteConsequences(wf: WorkflowDefinitionDto): string {
  const stages = `${wf.nodes.length} stage${wf.nodes.length === 1 ? "" : "s"}`;
  return (
    `“${wf.name}” and its ${stages} are removed from this chat for everyone, so nobody can start it again. ` +
    `Runs already going finish, and past runs stay in the history.`
  );
}

/// Stands in for a deleted workflow while it can still be brought back.
function DeletedRow({ name, onUndo }: { name: string; onUndo: () => void }) {
  return (
    <div
      role="status"
      className="ligus-enter flex items-center gap-2 rounded-xl border border-dashed px-3 py-2 text-xs text-ink-soft"
      style={{ borderColor: "var(--ligus-line)" }}
      data-testid="workflow-deleted"
    >
      <span className="min-w-0 flex-1 truncate">Deleted “{name}”.</span>
      <LinkButton onClick={onUndo}>Undo</LinkButton>
    </div>
  );
}

type Open = "none" | "run" | "change";

function DefinitionCard({
  sessionId,
  wf,
  agentName,
  onDelete,
}: {
  sessionId: string;
  wf: WorkflowDefinitionDto;
  agentName: (agentId: string) => string | undefined;
  onDelete: () => void;
}) {
  const qc = useQueryClient();
  const [open, setOpen] = useState<Open>("none");
  const [showStages, setShowStages] = useState(false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  function toggle(which: Open) {
    setOpen((o) => (o === which ? "none" : which));
    setText("");
  }

  async function act(fn: () => Promise<unknown>, after?: () => void) {
    setBusy(true);
    try {
      await fn();
      after?.();
      qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  function submit() {
    const value = text.trim();
    if (!value) return;
    if (open === "run") {
      void act(() => startWorkflowRun(sessionId, wf.id, value), () => toggle("run"));
    } else if (open === "change") {
      void act(
        async () => {
          const message = await workflowChangeRequest(sessionId, wf.id, value);
          await sendMessage(sessionId, message);
        },
        () => {
          toggle("change");
          toast.success("Asked the agent. Its revision will arrive in the Review pane.");
          qc.invalidateQueries({ queryKey: ["chat", sessionId] });
        },
      );
    }
  }

  return (
    <div className="rounded-xl border px-3 py-2.5" style={panelStyle}>
      <button
        type="button"
        className="flex w-full min-w-0 items-start gap-1.5 rounded-md text-left"
        onClick={() => setShowStages((v) => !v)}
        aria-expanded={showStages}
      >
        <span
          aria-hidden
          className="mt-0.5 inline-flex shrink-0 text-ink-faint transition-transform"
          style={{ transform: showStages ? "rotate(90deg)" : undefined }}
        >
          <IconChevronRight size={13} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-1.5">
            <span className="text-[13.5px] font-semibold leading-snug">{wf.name}</span>
            {wf.disabled && <Badge tone="warn">paused</Badge>}
          </span>
          <span className="block text-xs text-ink-soft">
            {wf.nodes.length} stage{wf.nodes.length === 1 ? "" : "s"} · {showStages ? "hide" : "show"} stages
          </span>
        </span>
      </button>
      {wf.description && <p className="mt-1 text-xs leading-[1.5] text-ink-soft">{wf.description}</p>}
      {showStages && <WorkflowStages nodes={wf.nodes} agentName={agentName} />}
      <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
        <Button variant="primary" disabled={busy || wf.disabled} onClick={() => toggle("run")}>
          Run
        </Button>
        <Button
          disabled={busy}
          aria-label={`${wf.disabled ? "Resume" : "Pause"} workflow ${wf.name}`}
          onClick={() => act(() => setWorkflowEnabled(sessionId, wf.id, wf.disabled))}
        >
          {wf.disabled ? "Resume" : "Pause"}
        </Button>
        <span className="flex-1" />
        <MoreMenu
          label={`More actions for workflow ${wf.name}`}
          disabled={busy}
          items={[
            {
              label: "Delete workflow…",
              danger: true,
              onSelect: () =>
                void confirmDialog(workflowDeleteConsequences(wf), {
                  title: `Delete “${wf.name}”?`,
                  confirmLabel: "Delete workflow",
                  danger: true,
                }).then((ok) => ok && onDelete()),
            },
          ]}
        />
      </div>
      {open !== "none" && (
        <div className="mt-2 flex gap-2">
          <input
            autoFocus
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
            placeholder={
              open === "run" ? (wf.inputLabel ?? "What should this run do?") : "What should change?"
            }
            aria-label={open === "run" ? "Run input" : "Requested change"}
            className="min-w-0 flex-1 rounded-xl border px-3 py-1.5 text-sm outline-none"
            style={fieldStyle}
          />
          <Button variant="primary" disabled={busy || !text.trim()} onClick={submit}>
            {open === "run" ? "Start" : "Ask"}
          </Button>
        </div>
      )}
      {open !== "change" && (
        <button
          type="button"
          className="mt-2 text-xs font-medium hover:underline"
          style={{ color: "var(--ligus-accent-ink)" }}
          onClick={() => toggle("change")}
          disabled={busy}
        >
          Ask an agent to change this
        </button>
      )}
    </div>
  );
}

function RunCard({ sessionId, run, hosts }: { sessionId: string; run: WorkflowRunDto; hosts: WorkspaceHostDto[] }) {
  const qc = useQueryClient();
  const live = run.status === "running" || run.status === "awaitingGate";
  // A resumable run in an interruptible state whose in-process driver is gone
  // (e.g. after an app restart). When the driver is alive, Resume would only
  // return "already being driven", so hide it.
  const resumable = live && !run.driverAlive;
  const tone = statusTone(run.status);

  async function act(fn: () => Promise<void>) {
    try {
      await fn();
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
      qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  return (
    <div className="rounded-xl border px-4 py-3" style={panelStyle}>
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate font-semibold">{run.definitionName}</div>
          <div className="truncate text-xs text-ink-soft">{run.input}</div>
        </div>
        <span
          className="shrink-0 rounded-lg px-2 py-0.5 text-[11px] font-medium"
          style={tone}
        >
          {statusLabel(run.status)}
        </span>
      </div>

      <div className="mt-2 flex flex-wrap gap-1.5">
        {run.nodes.map((n) => (
          <NodeChip key={n.nodeId} node={n} />
        ))}
      </div>

      {run.nodes
        .filter((n) => n.status === "awaitingApproval" && n.proposalId)
        .map((n) => (
          <div key={n.nodeId} className="mt-2 flex items-center gap-2 text-xs">
            <span className="text-ink-soft">“{n.name}” needs your decision:</span>
            <button
              className="rounded-lg px-2.5 py-1 font-medium"
              style={{ background: "color-mix(in srgb, var(--ligus-success) 20%, transparent)", color: "var(--ligus-success-ink)" }}
              onClick={() => act(async () => void (await voteProposal(sessionId, n.proposalId!, true)))}
            >
              Approve
            </button>
            <button
              className="rounded-lg px-2.5 py-1 font-medium"
              style={{ background: "color-mix(in srgb, var(--ligus-danger) 18%, transparent)", color: "var(--ligus-danger-ink)" }}
              onClick={() => act(async () => void (await voteProposal(sessionId, n.proposalId!, false)))}
            >
              Reject
            </button>
          </div>
        ))}

      {live && run.lease && (
        <RunLeaseLine
          lease={run.lease}
          hosts={hosts}
          onHandOver={(successor) =>
            act(async () => {
              await handOverWorkflowRun(sessionId, run.id, successor);
              toast.success("Handed over; the other device picks it up on its next heartbeat");
            })
          }
        />
      )}

      {run.nodes.some((n) => n.error) && (
        <div className="mt-2 text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
          {run.nodes.find((n) => n.error)?.error}
        </div>
      )}

      {live && (
        <div className="mt-2 flex gap-2 text-xs">
          <Button onClick={() => act(() => cancelWorkflowRun(sessionId, run.id))}>Cancel</Button>
          {resumable && (
            <Button onClick={() => act(() => resumeWorkflowRun(sessionId, run.id))}>Resume</Button>
          )}
        </div>
      )}
    </div>
  );
}

function NodeChip({ node }: { node: WorkflowNodeRunDto }) {
  const tone = statusTone(node.status);
  const [open, setOpen] = useState(false);
  // A stage's output/error was only reachable via the native hover tooltip.
  // Make the chip click-to-expand so a run is actually readable — the stage's
  // status, attempts, error, and output excerpt inline (a full run-log view).
  const hasDetail = Boolean(node.outputExcerpt || node.error || node.attempts > 1);
  return (
    <span className="inline-flex flex-col">
      <button
        type="button"
        onClick={() => hasDetail && setOpen((o) => !o)}
        className="rounded-lg px-2 py-0.5 text-left text-[11px] font-medium"
        style={{
          ...tone,
          textDecoration: node.status === "skipped" ? "line-through" : undefined,
          cursor: hasDetail ? "pointer" : "default",
        }}
        aria-expanded={hasDetail ? open : undefined}
        data-tip={`${node.name}: ${statusLabel(node.status)}`}
      >
        {node.kind === "gate" ? <span aria-label="approval gate" className="mr-1 inline-flex align-[-2px]"><IconLock size={11} /></span> : null}
        {node.name}
        {node.attempts > 1 ? ` ×${node.attempts}` : ""}
      </button>
      {open && (
        <div
          className="mt-1 max-w-full rounded-lg border p-2 text-[11px] leading-[1.5]"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-overlay)" }}
        >
          <div className="text-ink-soft">
            {statusLabel(node.status)}
            {node.attempts > 1 ? ` · ${node.attempts} attempts` : ""}
          </div>
          {node.error && (
            <div className="mt-1 whitespace-pre-wrap break-words" style={{ color: "var(--ligus-danger-ink)" }}>
              {node.error}
            </div>
          )}
          {node.outputExcerpt && (
            <div className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words text-ink-soft">
              {node.outputExcerpt}
            </div>
          )}
        </div>
      )}
    </span>
  );
}
