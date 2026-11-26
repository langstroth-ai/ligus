import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ackSandboxGrants,
  getSandboxPolicy,
  setSandboxGrants,
  setSandboxGrantsHere,
  type AgentGrant,
  type EffectivePolicy,
  type SandboxGrants,
} from "@/lib/ipc";
import { InfoTip, Button, Field, Section, Switch, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { SELECT_CLASS } from "@/components/settings/shared";
import { SANDBOX_AGENT_EVENT, takeSandboxAgentRequest } from "@/lib/nav";

type AgentDraft = { ro: string; rw: string; shell: boolean; localServers: boolean };

/// One path per line, trimmed, blanks dropped.
export function parsePaths(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
}

/// A path the backend will accept: `~/...` or absolute (client-side hint).
export function pathLooksOk(p: string): boolean {
  return (p.startsWith("~/") && p.length > 2) || (p.startsWith("/") && p.length > 1);
}

/// The grants with empty lists and empty agents left out (the wire shape).
export function normalizeGrants(g: SandboxGrants): SandboxGrants {
  const out: SandboxGrants = {};
  const ws = { readOnly: g.workspace?.readOnly ?? [], readWrite: g.workspace?.readWrite ?? [] };
  if (ws.readOnly.length || ws.readWrite.length) {
    out.workspace = {};
    if (ws.readOnly.length) out.workspace.readOnly = ws.readOnly;
    if (ws.readWrite.length) out.workspace.readWrite = ws.readWrite;
  }
  const agents: Record<string, AgentGrant> = {};
  for (const [id, a] of Object.entries(g.agents ?? {})) {
    const x: AgentGrant = {};
    if (a.readOnly?.length) x.readOnly = a.readOnly;
    if (a.readWrite?.length) x.readWrite = a.readWrite;
    if (a.shell) x.shell = true;
    if (a.localServers) x.localServers = true;
    if (Object.keys(x).length) agents[id] = x;
  }
  if (Object.keys(agents).length) out.agents = agents;
  return out;
}

const ACCESS_LABEL: Record<string, string> = { denied: "Denied", read: "Read", "read-write": "Read & write" };

function PolicyTable({ p }: { p: EffectivePolicy }) {
  return (
    <div className="border-t px-1 pt-2" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="mb-2 flex items-baseline justify-between gap-2">
        <span className="text-[12.5px] font-semibold">{p.runtime}</span>
        <span className="text-[11px]" style={{ color: "var(--ligus-ink-soft)" }}>
          sandbox: {p.tier}
        </span>
      </div>
      <table className="w-full text-[11.5px]">
        <thead>
          <tr style={{ color: "var(--ligus-ink-soft)" }}>
            <th className="w-[90px] pb-1 text-left font-semibold">Access</th>
            <th className="pb-1 text-left font-semibold">Path</th>
            <th className="pb-1 text-left font-semibold">Why</th>
          </tr>
        </thead>
        <tbody>
          {p.entries.map((e, i) => (
            <tr key={`${e.access}:${e.path}:${i}`} className="align-top">
              <td className="py-0.5 pr-2" style={{ color: e.access === "denied" ? "var(--ligus-danger, #c0392b)" : undefined }}>
                {ACCESS_LABEL[e.access] ?? e.access}
              </td>
              <td className="break-all py-0.5 pr-2 font-mono">{e.path}</td>
              <td className="py-0.5" style={{ color: "var(--ligus-ink-soft)" }}>
                {e.why}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="mt-2 text-[11.5px]">
        <span className="font-semibold">Shell:</span> {p.shell}
      </p>
      {p.localServers && (
        <p className="mt-1 text-[11.5px]">
          <span className="font-semibold">Local servers:</span> {p.localServers}
        </p>
      )}
    </div>
  );
}

function PathsField({
  label,
  value,
  onChange,
  disabled,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const bad = parsePaths(value).filter((p) => !pathLooksOk(p));
  return (
    <Field label={label} hint={bad.length ? `Not a ~/… or absolute path: ${bad.join(", ")}` : "One path per line: ~/… or absolute."}>
      <textarea
        aria-label={label}
        className="min-h-[56px] w-full rounded-xl border px-3 py-2 font-mono text-[12px]"
        style={fieldStyle}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
    </Field>
  );
}

/// Settings → Sandbox (Phase C): what agent turns may read and write on this
/// device (the home folder is denied by default), and the workspace's signed
/// grants: extra paths for every agent or one agent (Admin, with a passkey)
/// and an agent's shell for other members' turns (the Owner's).
export function SandboxSection() {
  const qc = useQueryClient();
  const [agentId, setAgentId] = useState<string>(() => takeSandboxAgentRequest() ?? "");
  // An agent row's "Sandbox…" while this section is already open.
  useEffect(() => {
    const on = (e: Event) => {
      takeSandboxAgentRequest();
      setAgentId((e as CustomEvent<string>).detail);
    };
    window.addEventListener(SANDBOX_AGENT_EVENT, on);
    return () => window.removeEventListener(SANDBOX_AGENT_EVENT, on);
  }, []);
  // Keep the last answer while another agent's policy loads, so the draft
  // below isn't reset by the switch.
  const q = useQuery({
    queryKey: ["sandbox-policy", agentId || null],
    queryFn: () => getSandboxPolicy(agentId || null),
    placeholderData: (prev) => prev,
  });
  const d = q.data;

  // The editable draft, reset whenever the grants in force change.
  const [wsRo, setWsRo] = useState("");
  const [wsRw, setWsRw] = useState("");
  const [agents, setAgents] = useState<Record<string, AgentDraft>>({});
  const grantsKey = useMemo(() => (d ? JSON.stringify(d.grants) : null), [d]);
  useEffect(() => {
    if (grantsKey === null) return;
    const g = d?.grants ?? {};
    setWsRo((g.workspace?.readOnly ?? []).join("\n"));
    setWsRw((g.workspace?.readWrite ?? []).join("\n"));
    const a: Record<string, AgentDraft> = {};
    for (const [id, x] of Object.entries(g.agents ?? {})) {
      a[id] = { ro: (x.readOnly ?? []).join("\n"), rw: (x.readWrite ?? []).join("\n"), shell: !!x.shell, localServers: !!x.localServers };
    }
    setAgents(a);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grantsKey]);

  const draft = (): SandboxGrants => {
    const g: SandboxGrants = { workspace: { readOnly: parsePaths(wsRo), readWrite: parsePaths(wsRw) }, agents: {} };
    for (const [id, a] of Object.entries(agents)) {
      g.agents![id] = { readOnly: parsePaths(a.ro), readWrite: parsePaths(a.rw), shell: a.shell, localServers: a.localServers };
    }
    return normalizeGrants(g);
  };
  const dirty = d ? JSON.stringify(draft()) !== JSON.stringify(normalizeGrants(d.grants)) : false;

  const save = useMutation({
    mutationFn: () => setSandboxGrants(draft()),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["sandbox-policy"] });
      toast.success("Sandbox grants saved");
    },
    onError: (e) => toast.error(`Couldn't save the sandbox grants: ${errMsg(e)}`),
  });

  const here = useMutation({
    mutationFn: (apply: boolean | null) => setSandboxGrantsHere(apply),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["sandbox-policy"] }),
    onError: (e) => toast.error(`Couldn't change this device's setting: ${errMsg(e)}`),
  });
  const ack = useMutation({
    mutationFn: ackSandboxGrants,
    onSuccess: () => qc.invalidateQueries({ queryKey: ["sandbox-policy"] }),
  });

  if (q.isError) {
    return (
      <Section title="Sandbox">
        <p className="text-[12.5px]">Couldn't load the sandbox policy: {errMsg(q.error)}</p>
      </Section>
    );
  }
  if (!d) {
    return (
      <Section title="Sandbox">
        <p className="text-[12.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
          Loading…
        </p>
      </Section>
    );
  }

  const agent = d.agents.find((a) => a.id === agentId);
  const cur: AgentDraft = (agentId && agents[agentId]) || { ro: "", rw: "", shell: false, localServers: false };
  const setCur = (patch: Partial<AgentDraft>) =>
    setAgents((prev) => ({ ...prev, [agentId]: { ...cur, ...patch } }));

  return (
    <>
      <Section
        title="Sandbox"
        info="Each turn gets its own worktree, scratch dir and home, and your keys, credentials and Ligus's data stay out of reach whatever a grant says."
      >
        <p className="text-[12.5px]">Agents can't read or write your home folder except the paths listed below.</p>
        <Field label="Show the policy for">
          <select
            aria-label="Show the policy for"
            className={`${SELECT_CLASS} w-full`}
            style={fieldStyle}
            value={agentId}
            onChange={(e) => setAgentId(e.target.value)}
          >
            <option value="">Every runtime this workspace uses</option>
            {d.agents.map((a) => (
              <option key={a.id} value={a.id}>
                @{a.name} ({a.runtime})
              </option>
            ))}
          </select>
        </Field>
        {d.policies.map((p) => (
          <PolicyTable key={p.runtime} p={p} />
        ))}
        {d.local && (
          <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
            This is your local workspace: you are its only person, so your agents always keep their shell.
          </p>
        )}
      </Section>

      <Section
        title="Signed grants"
        info="Grants are signed into the workspace and reach every device that runs these agents and applies them. Changing paths needs an Admin and a passkey; only the workspace Owner grants an agent the shell for other members' turns or lets it run local servers."
        action={
          <Button variant="primary" disabled={!d.canEditPaths || !dirty || save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? "Confirm with your passkey…" : "Save"}
          </Button>
        }
      >
        {!d.canEditPaths && (
          <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
            You can view them; an Admin changes them.
          </p>
        )}
        {d.changed && (
          <div
            role="status"
            className="flex items-center justify-between gap-2 rounded-xl border px-3 py-2 text-[12.5px]"
            style={{ borderColor: "var(--ligus-accent-ink)" }}
          >
            <span>These grants changed since you last looked.</span>
            <Button onClick={() => ack.mutate()}>Got it</Button>
          </div>
        )}
        <div className="space-y-1 border-y px-1 py-2" style={{ borderColor: "var(--ligus-line)" }}>
          <div className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-1.5">
              <span className="text-[12.5px] font-semibold">Apply workspace sandbox grants on this device</span>
              <InfoTip
                label="Apply workspace sandbox grants"
                text="By default the workspace Owner's own devices apply them and every other member's device doesn't. The policy above shows what applies here."
              />
            </span>
            <Switch
              on={d.appliesHere}
              onChange={(v) => here.mutate(v === d.appliesByDefault ? null : v)}
              label="Apply workspace sandbox grants on this device"
              disabled={here.isPending}
            />
          </div>
          <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {d.appliesWhy}. A grant opens paths (and an agent's shell) on your computer, so it's your choice.
          </p>
        </div>
        <PathsField label="Every agent: read-only" value={wsRo} onChange={setWsRo} disabled={!d.canEditPaths} />
        <PathsField label="Every agent: read and write" value={wsRw} onChange={setWsRw} disabled={!d.canEditPaths} />
        {agent ? (
          <div className="space-y-2.5 border-l pl-3" style={{ borderColor: "var(--ligus-line)" }}>
            <span className="text-[12.5px] font-semibold">@{agent.name} only</span>
            <PathsField label={`@${agent.name}: read-only`} value={cur.ro} onChange={(v) => setCur({ ro: v })} disabled={!d.canEditPaths} />
            <PathsField label={`@${agent.name}: read and write`} value={cur.rw} onChange={(v) => setCur({ rw: v })} disabled={!d.canEditPaths} />
            <div
              className="flex items-center justify-between gap-2"
              data-tip={d.canEditShell ? undefined : "Only the workspace Owner grants the shell"}
            >
              <span className="text-[12.5px]">Keep the shell for other members' turns</span>
              <Switch
                on={cur.shell}
                onChange={(v) => setCur({ shell: v })}
                label={`Keep the shell for other members' turns (@${agent.name})`}
                disabled={!d.canEditShell}
              />
            </div>
            <div
              className="flex items-center justify-between gap-2"
              data-tip={d.canEditShell ? undefined : "Only the workspace Owner lets an agent run local servers"}
            >
              <span className="flex items-center gap-1.5">
                <span className="text-[12.5px]">May run local servers</span>
                <InfoTip
                  label="May run local servers"
                  text="Some agent CLIs run their own helper server (agy does) and fail without it. Linux with bubblewrap: each turn has its own loopback, where that works; the Landlock tier keeps it off. Not available on macOS yet: the Mac's sandbox can't keep a listener loopback-only, so here it has no effect. Files, keys, unix sockets and egress never change."
                />
              </span>
              <Switch
                on={cur.localServers}
                onChange={(v) => setCur({ localServers: v })}
                label={`May run local servers (@${agent.name})`}
                disabled={!d.canEditShell}
              />
            </div>
            {d.policies.some((p) => p.tier === "seatbelt") && (
              <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
                Not available on macOS yet: the Mac's sandbox can't keep a listener loopback-only, so it has no effect on this Mac.
              </p>
            )}
          </div>
        ) : (
          <p className="text-[12px]" style={{ color: "var(--ligus-ink-soft)" }}>
            Pick an agent above for its own paths.
          </p>
        )}
      </Section>
    </>
  );
}
