// Agent access controls in an agent's settings (the workspace roster row):
// who can use the agent, its daily budgets, "notify me", and its usage as
// this device counted it. Editable only by the agent's owner (an Admin on a
// shared agent); everyone else sees a read-only badge.
import { useEffect, useId, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getAgentUsage, setAgentAccess, type WorkspaceAgentDto } from "@/lib/ipc";
import type { AgentUsageRowDto } from "@/bindings/AgentUsageRowDto";
import { InfoTip, SelectField } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

const ACCESS_HINT = "Turns run on its owner's device or host, on their credentials.";
const CAP_HINT =
  "Your own turns are never limited; the cap stops other members' turns once the day's estimated spend reaches it.";

/// The read-only label for who can use an agent.
export function accessLabel(agent: WorkspaceAgentDto): string {
  const shared = !agent.ownerActorId;
  if (agent.access === "everyone") return "Everyone";
  return shared ? "Admins only" : "Only its owner";
}

function usd(cents: number | null): string {
  return cents == null ? "" : (cents / 100).toFixed(2);
}

/// Dollars typed by a person → cents; `null` for empty; `undefined` when it
/// isn't a valid amount.
export function parseUsdCents(text: string): number | null | undefined {
  const t = text.trim().replace(/^\$/, "");
  if (!t) return null;
  if (!/^\d+(\.\d{0,2})?$/.test(t)) return undefined;
  return Math.round(parseFloat(t) * 100);
}

export function AgentAccessPanel({ sessionId, agent }: { sessionId: string; agent: WorkspaceAgentDto }) {
  if (!agent.canManageAccess) {
    return (
      <span
        className="inline-flex items-center rounded-full px-2 py-0.5 text-[11px]"
        style={{ background: "var(--ligus-mist)", color: "var(--ligus-ink-soft)" }}
        data-tip={ACCESS_HINT}
        aria-label={`Who can use this agent: ${accessLabel(agent)}`}
      >
        {accessLabel(agent)}
      </span>
    );
  }
  return <AccessDisclosure sessionId={sessionId} agent={agent} />;
}

/// One line for an agent's access, e.g. "Only me · 50 turns/day · $5.00 cap".
export function accessSummary(agent: WorkspaceAgentDto): string {
  const shared = !agent.ownerActorId;
  const who = agent.access === "everyone" ? "Everyone" : shared ? "Admins only" : "Only me";
  const t = agent.memberTurnsPerDay;
  const turns =
    t === 0 ? "unlimited turns" : t != null ? `${t} turns/day` : shared ? "no turn limit" : `${agent.effectiveMemberTurnsPerDay ?? 50} turns/day`;
  const cap = agent.usdCentsPerDay != null ? ` · $${usd(agent.usdCentsPerDay)} cap` : "";
  return `${who} · ${turns}${cap}`;
}

/// The editor stays folded under a one-line summary so it doesn't dominate
/// the roster row; "Access" opens it.
function AccessDisclosure({ sessionId, agent }: { sessionId: string; agent: WorkspaceAgentDto }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="mt-1 text-xs">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex min-w-0 items-baseline gap-1.5 rounded text-left hover:underline"
      >
        <span className="shrink-0 font-medium" style={{ color: "var(--ligus-accent-ink)" }}>
          Access
        </span>
        <span className="min-w-0 truncate text-ink-soft">{accessSummary(agent)}</span>
      </button>
      {open && <AgentAccessEditor sessionId={sessionId} agent={agent} />}
    </div>
  );
}

export function AgentAccessEditor({ sessionId, agent }: { sessionId: string; agent: WorkspaceAgentDto }) {
  const qc = useQueryClient();
  const ids = useId();
  const shared = !agent.ownerActorId;
  const [access, setAccess] = useState<"owner" | "everyone">(agent.access === "everyone" ? "everyone" : "owner");
  const [turnsMode, setTurnsMode] = useState<"default" | "unlimited" | "custom">(
    agent.memberTurnsPerDay == null ? "default" : agent.memberTurnsPerDay === 0 ? "unlimited" : "custom",
  );
  const [turns, setTurns] = useState(agent.memberTurnsPerDay && agent.memberTurnsPerDay > 0 ? String(agent.memberTurnsPerDay) : "");
  const [cap, setCap] = useState(usd(agent.usdCentsPerDay));
  const [notify, setNotify] = useState(agent.notifyOnUse);
  const [showUsage, setShowUsage] = useState(false);

  useEffect(() => {
    setAccess(agent.access === "everyone" ? "everyone" : "owner");
    setNotify(agent.notifyOnUse);
  }, [agent.access, agent.notifyOnUse]);

  const save = useMutation({
    mutationFn: async () => {
      let memberTurns: number | null = null;
      if (turnsMode === "unlimited") memberTurns = 0;
      if (turnsMode === "custom") {
        const n = Number.parseInt(turns, 10);
        if (!Number.isFinite(n) || n < 1) throw new Error("turns per member per day must be a whole number of at least 1");
        memberTurns = n;
      }
      const cents = parseUsdCents(cap);
      if (cents === undefined) throw new Error("the daily cap must be an amount in dollars, like 5 or 2.50");
      await setAgentAccess(agent.id, access, memberTurns, cents, notify);
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["agents", sessionId] });
      toast.success("Agent access saved.");
    },
    onError: (e) => toast.error(`Couldn't save agent access: ${errMsg(e)}`),
  });

  const defaultTurns = shared ? "no limit" : `${agent.effectiveMemberTurnsPerDay ?? 50}`;
  return (
    <div className="mt-2 space-y-2 text-xs">
      <div>
        <span className="mb-1 flex items-center gap-1.5 font-medium">
          Who can use this agent
          <InfoTip label="who can use this agent" text={ACCESS_HINT} />
        </span>
        <div>
          <SelectField value={access} onChange={(v) => setAccess(v === "everyone" ? "everyone" : "owner")} ariaLabel="Who can use this agent">
            <option value="owner">{shared ? "Admins only" : "Only me"}</option>
            <option value="everyone">Everyone in the workspace</option>
          </SelectField>
        </div>
      </div>
      <div className="grid grid-cols-2 gap-2">
        <div>
          <span className="mb-1 block font-medium">Turns per member per day</span>
          <SelectField value={turnsMode} onChange={(v) => setTurnsMode(v as typeof turnsMode)} ariaLabel="Turns per member per day">
            <option value="default">Default ({defaultTurns})</option>
            <option value="unlimited">Unlimited</option>
            <option value="custom">Custom…</option>
          </SelectField>
          {turnsMode === "custom" && (
            <input
              aria-label="Turns per member per day (number)"
              inputMode="numeric"
              value={turns}
              onChange={(e) => setTurns(e.target.value.replace(/[^\d]/g, ""))}
              placeholder="e.g. 20"
              className="mt-1 w-full rounded-xl border px-3 py-1.5 outline-none"
              style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)" }}
            />
          )}
        </div>
        <div>
          <span className="mb-1 flex items-center gap-1.5">
            <label htmlFor={`${ids}-cap`} className="font-medium">
              Daily cap (USD)
            </label>
            <InfoTip label="the daily cap" text={CAP_HINT} />
          </span>
          <input
            id={`${ids}-cap`}
            aria-label="Daily spending cap in US dollars"
            inputMode="decimal"
            value={cap}
            onChange={(e) => setCap(e.target.value)}
            placeholder="No cap"
            className="w-full rounded-xl border px-3 py-1.5 outline-none"
            style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)" }}
          />
        </div>
      </div>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={notify} onChange={(e) => setNotify(e.target.checked)} />
        Notify me when someone else uses this agent
      </label>
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={() => save.mutate()}
          disabled={save.isPending}
          className="rounded-full px-3 py-1 font-semibold"
          style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
        >
          {save.isPending ? "Saving…" : "Save access"}
        </button>
        <button
          type="button"
          aria-expanded={showUsage}
          onClick={() => setShowUsage((v) => !v)}
          className="text-ink-soft hover:opacity-80"
        >
          {showUsage ? "Hide usage on this device" : "Usage on this device"}
        </button>
      </div>
      {showUsage && <UsageTable agentId={agent.id} />}
    </div>
  );
}

function cost(r: AgentUsageRowDto): string {
  if (r.estimatedCostUsd == null) return "not reported";
  const s = `$${r.estimatedCostUsd.toFixed(2)}`;
  return r.uncostedTurns > 0 ? `${s} + ${r.uncostedTurns} not reported` : s;
}

function UsageTable({ agentId }: { agentId: string }) {
  const usage = useQuery({ queryKey: ["agent-usage", agentId], queryFn: () => getAgentUsage(agentId) });
  if (usage.isLoading) return <p className="text-ink-soft">Loading usage…</p>;
  if (usage.error) return <p style={{ color: "var(--ligus-danger-ink)" }}>Couldn't load usage: {errMsg(usage.error)}</p>;
  const d = usage.data;
  if (!d) return null;
  const members = new Map<string, { name: string; today?: AgentUsageRowDto; month?: AgentUsageRowDto }>();
  for (const r of d.month) members.set(r.member, { name: r.memberName, month: r });
  for (const r of d.today) members.set(r.member, { ...(members.get(r.member) ?? { name: r.memberName }), today: r });
  const rows = [...members.values()];
  return (
    <div>
      {rows.length === 0 ? (
        <p className="text-ink-soft">No turns on this device this month.</p>
      ) : (
        <table className="w-full text-left" aria-label="Agent usage on this device">
          <thead>
            <tr className="text-ink-soft">
              <th className="py-1 font-medium">Member</th>
              <th className="py-1 font-medium">Today</th>
              <th className="py-1 font-medium">This month</th>
              <th className="py-1 font-medium">Est. cost (month)</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.name + (r.month?.member ?? r.today?.member ?? "")}>
                <td className="py-0.5">{r.name}</td>
                <td className="py-0.5">{r.today?.turns ?? 0}</td>
                <td className="py-0.5">{r.month?.turns ?? 0}</td>
                <td className="py-0.5">{r.month ? cost(r.month) : "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="mt-1 text-ink-soft">
        Days are UTC ({d.day}). Counted where the agent runs; subscription CLIs that report no usage are counted in turns only.
      </p>
    </div>
  );
}
