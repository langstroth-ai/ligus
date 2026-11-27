// The dispatch outcome under a person's own message (workspace model v3,
// 5.4): one quiet line when its turn doesn't run yet — queued for a host
// that is offline, the host offline or gone, or can't run and why — with
// "Run here this time" as the one visible action when this device may run
// it. Anything else ("Use Claude Code instead" for this one turn) is behind
// the row's ⋯ menu. Nothing runs anywhere else on its own.

import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { TurnRouteDto } from "@/bindings/TurnRouteDto";
import type { ReplyOutcomeDto } from "@/bindings/ReplyOutcomeDto";
import { runTurnHere } from "@/lib/ipc";
import { useRowMenu, type RowAction } from "@/components/RowMenu";
import { LinkButton } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

export function TurnRouteRow({
  sessionId,
  messageId,
  route,
}: {
  sessionId: string;
  messageId: string;
  route: TurnRouteDto;
}) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const run = async (useClaudeCode: boolean) => {
    setBusy(true);
    try {
      await runTurnHere(sessionId, messageId, useClaudeCode);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
      void qc.invalidateQueries({ queryKey: ["chat", sessionId] });
    }
  };
  const actions: RowAction[] = route.useClaudeCode
    ? [{ label: "Use Claude Code instead", title: "Runs this one turn on Claude Code on this device", onSelect: () => void run(true) }]
    : [];
  const { rowProps, button, menu } = useRowMenu("Turn actions", actions);
  const dot = route.state === "queued" ? "var(--ligus-ink-soft)" : "var(--ligus-accent-warm, orange)";
  return (
    <div
      role="status"
      data-route-state={route.state}
      data-route-reason={route.reason}
      className="group flex items-center justify-end gap-2 py-0.5 pr-1 text-xs text-ink-soft"
      {...rowProps}
    >
      <span aria-hidden style={{ color: dot }}>
        ●
      </span>
      <span className="min-w-0">{route.text}</span>
      {route.runHere && (
        <>
          <span aria-hidden>·</span>
          <LinkButton disabled={busy} onClick={() => void run(false)} title="Runs this one turn on this device">
            Run here this time
          </LinkButton>
        </>
      )}
      {button}
      {menu}
    </div>
  );
}

/// How a finished reply ended, when it didn't post normally: one muted line
/// ("Canceled", "Failed"). A posted reply shows nothing extra; which model
/// answered rides the reply's own provenance.
export function ReplyOutcomeLine({ outcome }: { outcome: ReplyOutcomeDto | null | undefined }) {
  if (!outcome || outcome.status === "ok") return null;
  const label = outcome.status === "canceled" ? "Canceled" : "Failed";
  const where = [outcome.model, outcome.host].filter(Boolean).join(" on ");
  return (
    <div role="note" data-outcome={outcome.status} className="py-0.5 pl-11 text-xs text-ink-soft">
      {label}
      {where ? ` · ${where}` : ""}
    </div>
  );
}
