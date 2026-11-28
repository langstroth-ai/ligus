// Settings > Set up: the "Set up Ligus" checklist (the same rows as
// `ligus setup`, lt and the phone's Health), and Clean up. One line per row,
// each with at most one action; grey "Not set up" for what was never chosen,
// never red. Clean up lists the leftovers with one action each, and nothing
// is removed until the person confirms it (a removal then asks for the
// passkey, one ceremony per item).

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { setupChecklist, type CleanupItem, type SetupChecklist as Checklist, type SetupRow } from "@/lib/ipc";
import { Button, Section } from "@/components/ui";
import { confirmDialog } from "@/components/Dialog";
import { toast, errMsg } from "@/components/Toast";
import { BOX_UPGRADE_LINE, batchConfirm, goTo, rowActionTarget, rowMark, rowTone, runCleanup } from "@/lib/setupChecklist";

export const SETUP_CHECKLIST_KEY = ["setup-checklist"] as const;

function Mark({ row }: { row: SetupRow }) {
  return (
    <span aria-hidden className="w-4 shrink-0 text-center text-[13px]" style={{ color: rowTone(row.state) }}>
      {rowMark(row.state)}
    </span>
  );
}

function RowLine({ row, onAction, open }: { row: SetupRow; onAction: (row: SetupRow) => void; open: boolean }) {
  const greyed = row.state === "notSetUp";
  return (
    <div role="listitem" className="flex min-h-[36px] items-center gap-3 py-1.5" data-testid={`setup-row-${row.id}`} data-state={row.state}>
      <Mark row={row} />
      <span className="w-24 shrink-0 text-[13px] font-medium">{row.title}</span>
      <span className="min-w-0 flex-1 truncate text-[13px]" style={{ color: greyed ? "var(--ligus-ink-faint)" : "var(--ligus-ink-soft)" }} data-tip={row.detail}>
        {row.detail}
      </span>
      {row.action && (
        <Button size="sm" variant={row.state === "done" ? "quiet" : "ghost"} aria-expanded={open || undefined} onClick={() => onAction(row)}>
          {row.action.label}
        </Button>
      )}
    </div>
  );
}

/// Clean up: the leftovers, each with its one action, plus removing all the
/// removable ones in one go ("Confirm 1 of 3": still one passkey each).
export function CleanupList({ items, onChanged }: { items: CleanupItem[]; onChanged: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [step, setStep] = useState<string | null>(null);

  async function one(item: CleanupItem) {
    const ok = await confirmDialog(item.action.confirm, {
      title: item.action.id === "remove" ? "Remove" : "Remove from view",
      confirmLabel: item.action.label,
      danger: item.action.id === "remove",
    });
    if (!ok) return;
    setBusy(item.key);
    try {
      await runCleanup(item);
      toast.success(item.action.id === "remove" ? `Removed ${item.label}.` : `${item.label} is out of view on this device.`);
    } catch (e) {
      toast.error(`Couldn't ${item.action.id === "remove" ? "remove" : "hide"} ${item.label}: ${errMsg(e)}`);
    } finally {
      setBusy(null);
      onChanged();
    }
  }

  const removable = items.filter((i) => i.action.id === "remove");

  async function all() {
    if (!(await confirmDialog(batchConfirm(removable), { title: "Clean up", confirmLabel: "Remove", danger: true }))) return;
    let done = 0;
    for (const [n, item] of removable.entries()) {
      setStep(`Confirm ${n + 1} of ${removable.length}: ${item.label}`);
      setBusy(item.key);
      try {
        await runCleanup(item);
        done += 1;
      } catch (e) {
        toast.error(`Stopped at ${item.label}: ${errMsg(e)}`);
        break;
      }
    }
    setBusy(null);
    setStep(null);
    if (done > 0) toast.success(done === 1 ? "Removed 1." : `Removed ${done}.`);
    onChanged();
  }

  if (items.length === 0) {
    return <p className="text-[13px] text-ink-soft">Nothing to clean up.</p>;
  }
  return (
    <div className="space-y-2" data-testid="setup-cleanup">
      <ul className="divide-y" style={{ borderColor: "var(--ligus-line)" }}>
        {items.map((i) => (
          <li key={i.key} className="flex min-h-[36px] items-center gap-3 py-1.5" data-testid={`cleanup-${i.key}`}>
            <span className="min-w-0 flex-1 truncate text-[13px]">
              {i.label}
              {i.detail && <span className="text-ink-soft"> · {i.detail}</span>}
            </span>
            <Button size="sm" variant={i.action.id === "remove" ? "ghost" : "quiet"} disabled={busy !== null} onClick={() => void one(i)}>
              {busy === i.key ? "…" : i.action.label}
            </Button>
          </li>
        ))}
      </ul>
      {step && (
        <p role="status" className="text-[12px] text-ink-soft">
          {step}
        </p>
      )}
      {removable.length > 1 && (
        <Button size="sm" variant="quiet" disabled={busy !== null} onClick={() => void all()}>
          Remove all {removable.length}
        </Button>
      )}
    </div>
  );
}

/// The checklist, as Settings > Set up shows it. `initial` is for tests.
export function SetupChecklistPane({ initial }: { initial?: Checklist } = {}) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: SETUP_CHECKLIST_KEY, queryFn: setupChecklist, initialData: initial, refetchOnWindowFocus: true });
  const [open, setOpen] = useState<"cleanup" | "boxUpgrade" | null>(null);
  const refresh = () => void qc.invalidateQueries({ queryKey: SETUP_CHECKLIST_KEY });

  function act(row: SetupRow) {
    const t = rowActionTarget(row.action?.id ?? "");
    if (t.kind === "cleanup" || t.kind === "boxUpgrade") setOpen((o) => (o === t.kind ? null : t.kind));
    else goTo(t);
  }

  return (
    <Section
      title="Set up Ligus"
      info="One line for each part of your setup, the same on this desktop, in ligus setup, lt and your phone's Health. Grey means you haven't set it up (some parts are optional); a dot means it wants a look. Clean up lists what's left over and removes nothing until you confirm."
    >
      {q.isError && <p className="text-[13px] text-ink-soft">Couldn't read your setup: {errMsg(q.error)}</p>}
      {!q.data && !q.isError && <p className="text-[13px] text-ink-soft">Reading your setup…</p>}
      {q.data && (
        <div role="list" className="divide-y" style={{ borderColor: "var(--ligus-line)" }} data-testid="setup-checklist">
          {q.data.rows.map((r) => (
            <div key={r.id}>
              <RowLine row={r} onAction={act} open={(r.id === "cleanup" && open === "cleanup") || (r.id === "box" && open === "boxUpgrade")} />
              {r.id === "box" && open === "boxUpgrade" && (
                <p className="pb-2 pl-7 text-[12px] text-ink-soft" data-testid="setup-box-upgrade">
                  On the box, run <code>{BOX_UPGRADE_LINE}</code>: it upgrades, restarts and checks the box. Nothing upgrades on its own.
                </p>
              )}
              {r.id === "cleanup" && open === "cleanup" && (
                <div className="pb-2 pl-7">
                  <CleanupList items={q.data.cleanup} onChanged={refresh} />
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </Section>
  );
}
