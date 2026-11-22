import type { ReactNode } from "react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addAgent,
  listAgentTemplates,
  listMembershipAudit,
  workspaceInvite,
  listWorkspaces,
  removeMember,
  removeAndRevoke,
  setMemberRole,
  passkeyRegrant,
  workspaceMembers,
  workspaceRemoveMember,
  workspaceClaimMembership,
  workspaceListInvites,
  workspaceCreateInvite,
  workspaceRevokeInvite,
  workspaceJoinViaInvite,
  hasIssuerKey,
  generateAgentBootstrap,
  type InviteEntry,
  addVault,
  getContextTelemetry,
  implementProposal,
  askAgentToRebase,
  openChatCheckout,
  requestApplyOnHost,
  withdrawApplyRequest,
  installMcpServer,
  installSkill,
  listAgents,
  listMcpServers,
  listMembers,
  presenceList,
  listProposals,
  listQueuedWork,
  listRuntimes,
  listSkills,
  listVaults,
  listWorkspaceHosts,
  previewVault,
  removeAgent,
  setAgentAvatar,
  setAgentHost,
  removeMcpServer,
  removeSkill,
  removeVault,
  setChatRuntime,
  setMcpEnabled,
  syncStatus,
  onChatStream,
  voteProposal,
  refineProposal,
  requestProposalChanges,
  dismissProposals,
  type ProposalDto,
  type ApplyRequestDto,
  type RuntimeSummaryDto,
  type McpServerDto,
  type QueuedWorkDto,
  type WorkspaceHostDto,
  type WorkspaceAgentDto,
  deleteMemberAttachments,
  openExternal,
} from "@/lib/ipc";
import { docsUrl, PANE_DOCS } from "@/lib/docs";
import { confirmDialog } from "@/components/Dialog";
import { groupProposals, isSettled, isVotable, myVote } from "@/lib/proposals";
import { parseUnifiedDiff } from "@/lib/diff";
import { relTime } from "@/lib/time";
import { LogsView } from "@/components/LogsView";
import { Avatar } from "@/components/Avatar";
import { AgentAccessPanel } from "@/components/AgentAccessPanel";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { PRIMARY_NAME, PRIMARY_HANDLE } from "@/lib/agent";
import { toast, errMsg } from "@/components/Toast";
import { inviteError } from "@/lib/invites";
import { confirmThen } from "@/lib/confirm";
import { MoreMenu } from "@/components/MoreMenu";
import {
  Button,
  IconButton,
  Card,
  Modal,
  Section,
  Switch,
  Field,
  SelectField,
  FormDisclosure,
  EmptyHint,
  ErrorState,
  LoadingState,
  Badge,
  StatusDot,
  type Tone,
  SELECT_TINT,
  LinkButton,
} from "@/components/ui";
import { Markdown } from "@/components/Markdown";
import { Menu } from "@/components/Menu";
import {
  IconX,
  IconChevronDown,
  IconPlus,
  IconCheck,
  IconInbox,
  IconMonitor,
} from "@/lib/icons";
import { PANES, type UtilityPane } from "@/lib/panes";
import { WorkflowsPane, WorkflowStages } from "@/components/WorkflowsPane";
import { ProposalSets, useProposalSets } from "@/components/ProposalSets";
import { withoutSetAlternatives } from "@/lib/proposalSets";
import { FlowPane } from "@/components/flow/FlowPane";
import { ProposalBadges } from "@/components/ReviewBadges";
import { ImplementPreview } from "@/components/ImplementPreview";
import { AgentAsks } from "@/components/AgentAsks";
import { MemoryPane } from "@/components/MemoryPane";
import { hostGatewaysLabel } from "@/components/settings/GatewayPicker";

// Re-exported from the shared pane source so existing
// `import type { UtilityPane } from "@/components/RightRail"` keeps working.
export type { UtilityPane };

export function RightRail({
  width,
  sessionId,
  pane,
  activeRuntimeId,
  onChangePane,
  onClose,
  showPaneRail = true,
}: {
  width: number;
  sessionId: string;
  pane: UtilityPane;
  activeRuntimeId: string;
  onChangePane: (pane: UtilityPane) => void;
  /// Dismiss the whole utility pane (besides the header Tools toggle).
  onClose: () => void;
  /// Show the pane's own icon rail. Hidden when the sidebar is visible, since
  /// the sidebar's Workspace list already switches panes (F10).
  showPaneRail?: boolean;
}) {
  // Escape closes the pane — a keyboard alternative to the Tools toggle and the
  // header ✕. Ignored while typing so it never eats a composer/​input Escape.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      // Escape closes the innermost thing first: an open dialog, menu or
      // popover owns this press, not the rail behind it.
      if (document.querySelector('[aria-modal="true"], [role="menu"], [role="listbox"]')) return;
      const el = document.activeElement;
      const typing =
        el instanceof HTMLElement &&
        (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
      if (!typing) onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const panes = PANES;

  // Badge the Review icon with proposals/gates awaiting a decision, so the
  // pending count is visible even from the collapsed-sidebar icon rail.
  const railProposals = useQuery({
    queryKey: ["proposals", sessionId],
    queryFn: () => listProposals(sessionId),
    enabled: Boolean(sessionId),
  });
  const paneBadges: Partial<Record<UtilityPane, number>> = {
    review: groupProposals(railProposals.data ?? []).active.length,
  };

  return (
    <aside
      data-tour="right-rail"
      className="relative flex shrink-0 overflow-hidden border-l"
      style={{ width, borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
    >
      {showPaneRail && (
        <div
          className="flex w-10 shrink-0 flex-col items-center gap-2 border-r py-3"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-overlay)" }}
        >
          {panes.map((item) => (
            <button
              key={item.id}
              onClick={() => onChangePane(item.id)}
              className="ligus-press relative flex h-10 w-10 items-center justify-center rounded-2xl"
              style={{
                background: pane === item.id ? "var(--ligus-mist)" : "transparent",
                color: pane === item.id ? "var(--ligus-accent-ink)" : "var(--ligus-ink)",
                opacity: pane === item.id ? 1 : 0.72,
              }}
              data-tip={item.label}
              aria-label={`${item.label} pane`}
              data-pane={item.id}
              aria-pressed={pane === item.id}
            >
              {item.icon(17)}
              {(paneBadges[item.id] ?? 0) > 0 && (
                <span
                  className="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full px-1 text-[11px] font-semibold leading-none"
                  style={{ background: "var(--ligus-warm-fill)", color: "var(--ligus-on-accent)" }}
                  aria-label={`${paneBadges[item.id]} awaiting review`}
                >
                  {paneBadges[item.id]}
                </span>
              )}
            </button>
          ))}
        </div>
      )}
      {/* Close the pane — a second dismiss affordance besides the header Tools
          toggle (and Escape). Sits in the pane header's empty right edge. */}
      <button
        onClick={onClose}
        className="absolute right-2 top-2.5 z-10 flex h-7 w-7 items-center justify-center rounded-lg text-ink-soft transition-colors hover:text-ink"
        data-tip="Close panel (Esc)"
        aria-label="Close panel"
      >
        <IconX size={15} />
      </button>
      <div className="min-h-0 min-w-0 flex-1 overflow-hidden">
        {pane === "tools" && <ToolsPane sessionId={sessionId} activeRuntimeId={activeRuntimeId} />}
        {pane === "review" && <ReviewPane sessionId={sessionId} />}
        {pane === "people" && <PeoplePane sessionId={sessionId} />}
        {pane === "vaults" && <VaultsPane sessionId={sessionId} />}
        {pane === "skills" && <SkillsPane sessionId={sessionId} />}
        {pane === "workflows" && <WorkflowsPane sessionId={sessionId} />}
        {pane === "flow" && <FlowPane />}
        {pane === "memory" && <MemoryPane sessionId={sessionId} />}
        {pane === "context" && <ContextPane sessionId={sessionId} activeRuntimeId={activeRuntimeId} />}
        {pane === "activity" && <ActivityPane />}
      </div>
    </aside>
  );
}

// Scroll offsets per rail pane, so flipping Review → Flow → Review returns you
// to where you were instead of the top (module-level: survives remounts).
const railScroll = new Map<string, number>();

export function RailFrame({
  title,
  about,
  docs,
  children,
  actions,
}: {
  title: string;
  /// What the pane is for, in a sentence. Not shown on screen: it's the "?"
  /// link's tooltip and accessible name, so the pane opens on its content.
  about: string;
  /// The docs page the "?" link opens (a path under the docs site).
  docs: string;
  children: ReactNode;
  /// Optional header controls, right-aligned under the close button's row.
  actions?: ReactNode;
}) {
  const bodyRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    // Content often arrives a frame or two after mount (queries, lazy cards),
    // so keep re-applying the saved offset until it sticks, and don't record
    // the clamped offsets seen on the way.
    const target = railScroll.get(title) ?? 0;
    let restoring = target > 0;
    let tries = 0;
    let raf = 0;
    const tryRestore = () => {
      el.scrollTop = target;
      if (Math.abs(el.scrollTop - target) < 2 || ++tries > 30) restoring = false;
      else raf = requestAnimationFrame(tryRestore);
    };
    if (restoring) tryRestore();
    const save = () => {
      if (!restoring) railScroll.set(title, el.scrollTop);
    };
    const stop = () => (restoring = false);
    el.addEventListener("scroll", save, { passive: true });
    el.addEventListener("wheel", stop, { passive: true });
    el.addEventListener("pointerdown", stop);
    return () => {
      cancelAnimationFrame(raf);
      el.removeEventListener("scroll", save);
      el.removeEventListener("wheel", stop);
      el.removeEventListener("pointerdown", stop);
    };
  }, [title]);
  return (
    <div className="flex h-full flex-col">
      <div className="border-b px-4 pb-2.5 pt-3" style={{ borderColor: "var(--ligus-line)" }}>
        <div className="flex items-center gap-1.5 pr-8">
          <h2 className="ligus-title">{title}</h2>
          <PaneHelpLink title={title} about={about} docs={docs} />
        </div>
        {actions && <div className="mt-2">{actions}</div>}
      </div>
      <div ref={bodyRef} className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-3 py-3">
        {children}
      </div>
    </div>
  );
}

/// The one help affordance in every rail pane: a small "?" beside the title.
/// Hover (or focus) shows what the pane is for; clicking opens its docs page.
export function PaneHelpLink({ title, about, docs }: { title: string; about: string; docs: string }) {
  const url = docsUrl(docs);
  return (
    <a
      href={url}
      target="_blank"
      rel="noreferrer"
      data-tip={`${about} Opens the docs.`}
      aria-label={`About ${title}: ${about} Opens the docs.`}
      data-testid="pane-help"
      onClick={(e) => {
        // The desktop webview can't open a new window: hand it to the OS browser.
        e.preventDefault();
        void openExternal(url).catch(() => window.open(url, "_blank", "noopener"));
      }}
      className="grid h-[18px] w-[18px] shrink-0 place-items-center rounded-full border text-[11px] font-semibold leading-none text-ink-soft transition-colors hover:text-ink"
      style={{ borderColor: "var(--ligus-line)" }}
    >
      ?
    </a>
  );
}

// ─── Tools pane (design spec §6.3 / §6.4 / §7.5) ─────────────────────────────
// Scope is the top-level split: "In this chat" is READ-ONLY (what the agents
// here can reach, inherited from the workspace); "Workspace" is where those
// same records are actually edited. §11.1 made the workspace own configuration,
// so the chat side never renders an editable control — except the answering
// runtime, which is genuinely per-chat.

type ToolsScope = "chat" | "workspace";

function ToolsPane({
  sessionId,
  activeRuntimeId,
}: {
  sessionId: string;
  activeRuntimeId: string;
}) {
  const qc = useQueryClient();
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const agentTemplates = useQuery({ queryKey: ["agent-templates"], queryFn: listAgentTemplates });
  const agents = useQuery({ queryKey: ["agents", sessionId], queryFn: () => listAgents(sessionId) });
  const mcp = useQuery({ queryKey: ["mcp"], queryFn: listMcpServers });
  const hosts = useQuery({ queryKey: ["workspace-hosts"], queryFn: listWorkspaceHosts });
  const hostList = hosts.data ?? [];
  const workerHosts = hostList.filter((h) => h.kind === "worker");
  const [scope, setScope] = useState<ToolsScope>("chat");
  const [agentName, setAgentName] = useState("");
  const [agentRole, setAgentRole] = useState("");
  const [agentRuntimeId, setAgentRuntimeId] = useState(activeRuntimeId);
  const [agentHostId, setAgentHostId] = useState("");
  const [showAddAgent, setShowAddAgent] = useState(false);
  const [mcpSource, setMcpSource] = useState("");
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [showInstallMcp, setShowInstallMcp] = useState(false);

  useEffect(() => {
    if (!agentRuntimeId) {
      setAgentRuntimeId(activeRuntimeId || runtimes.data?.[0]?.id || "");
    }
  }, [activeRuntimeId, agentRuntimeId, runtimes.data]);

  const addAgentMutation = useMutation({
    mutationFn: async () => {
      const name = agentName.trim();
      await addAgent(sessionId, name, agentRuntimeId, agentRole.trim());
      // `add_agent` doesn't return the new id, and agent names are enforced
      // unique — so to bind the persona to a worker we refetch the roster and
      // match by name, then reassign its host via the existing path.
      if (agentHostId) {
        const list = await listAgents(sessionId);
        const created = list.find((a) => a.name.toLowerCase() === name.toLowerCase());
        if (created) await setAgentHost(created.id, agentHostId);
      }
    },
    onSuccess: () => {
      setAgentName("");
      setAgentRole("");
      setAgentHostId("");
      setShowAddAgent(false);
      qc.invalidateQueries({ queryKey: ["agents", sessionId] });
      qc.invalidateQueries({ queryKey: ["workspace-hosts"] });
      toast.success("Agent added.");
    },
    onError: (e) => toast.error(`Couldn't add agent: ${errMsg(e)}`),
  });
  const removeAgentMutation = useMutation({
    mutationFn: (agentId: string) => removeAgent(sessionId, agentId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["agents", sessionId] }),
    onError: (e) => toast.error(`Couldn't remove agent: ${errMsg(e)}`),
  });
  const installMcpMutation = useMutation({
    mutationFn: () => installMcpServer(mcpSource.trim()),
    onSuccess: () => {
      setMcpSource("");
      setMcpError(null);
      setShowInstallMcp(false);
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("MCP server installed (disabled until you enable it).");
    },
    onError: (error) => {
      setMcpError(String(error));
      toast.error(`Couldn't install MCP server: ${errMsg(error)}`);
    },
  });
  const removeMcpMutation = useMutation({
    mutationFn: (serverId: string) => removeMcpServer(serverId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
    onError: (e) => toast.error(`Couldn't remove MCP server: ${errMsg(e)}`),
  });

  const selectRuntime = async (id: string) => {
    try {
      await setChatRuntime(sessionId, id);
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
      qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast.error(`Couldn't switch runtime: ${errMsg(e)}`);
    }
  };
  const toggleMcp = async (server: McpServerDto, enabled: boolean) => {
    try {
      await setMcpEnabled(server.id, enabled);
      qc.invalidateQueries({ queryKey: ["mcp"] });
    } catch (e) {
      toast.error(`Couldn't ${enabled ? "enable" : "disable"} ${server.id}: ${errMsg(e)}`);
    }
  };

  const runtimeList = runtimes.data ?? [];
  const activeRuntime = runtimeList.find((r) => r.id === activeRuntimeId);
  const agentList = agents.data ?? [];
  const mcpList = mcp.data ?? [];
  const enabledMcp = mcpList.filter((s) => s.enabled);

  return (
    <RailFrame title="Tools" about="What the agents here can reach, and where it's configured." docs={PANE_DOCS.tools}>
      <ScopeToggle scope={scope} onScope={setScope} />

      {scope === "chat" ? (
        <>
          <div
            className="mb-4 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-xl border px-3 py-2.5 text-[12px] leading-5"
            style={{
              borderColor: "color-mix(in srgb, var(--ligus-accent-cool) 38%, transparent)",
              background: SELECT_TINT,
              color: "var(--ligus-ink)",
            }}
          >
            <span>These are inherited from the workspace.</span>
            <button
              onClick={() => setScope("workspace")}
              className="font-medium underline underline-offset-2 hover:opacity-80"
              style={{ color: "var(--ligus-accent-ink)" }}
            >
              Edit in Workspace
            </button>
          </div>

          <Section title="Answering runtime">
            {runtimeList.length === 0 ? (
              <EmptyHint text="No runtimes configured yet." />
            ) : (
              runtimeList.map((rt) => (
                <RuntimeCard
                  key={rt.id}
                  runtime={rt}
                  active={rt.id === activeRuntimeId}
                  onSelect={() => selectRuntime(rt.id)}
                />
              ))
            )}
          </Section>

          <Section title="Agents available to @mention">
            {/* The default agent (@ligus) always exists — it answers untagged
                messages and is bound to the active runtime — even before the
                user adds any of their own. Showing it here (and counting it in
                the sidebar) stops the pane claiming "no agents" while one is
                mid-reply (F1). It has no roster record, so no edit/remove. */}
            <Card className="flex items-center gap-2.5 px-3 py-2.5">
              <Avatar
                name={PRIMARY_NAME}
                kind="agent"
                host={activeRuntime?.location?.trim().toLowerCase() === "remote" ? "remote" : "local"}
                size={28}
              />
              <div className="min-w-0">
                <div className="truncate text-[13px] font-medium">@{PRIMARY_HANDLE}</div>
                <div className="truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                  {activeRuntime ? runtimePickerLabel(activeRuntime) : "default runtime"} · default
                </div>
              </div>
            </Card>
            {agentList.map((agent) => (
              <Card key={agent.id} className="flex items-center gap-2.5 px-3 py-2.5">
                <Avatar
                  name={agent.name}
                  url={agent.avatarUrl}
                  colorHex={agent.avatarColorHex}
                  kind="agent"
                  size={28}
                />
                <div className="min-w-0">
                  <div className="truncate text-[13px] font-medium">@{agent.name}</div>
                  <div className="truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                    {agent.runtimeId} · {agent.role || "agent"}
                  </div>
                  <div className="mt-0.5">
                    <AgentHostBadge hostId={agent.hostId} hosts={hostList} />
                  </div>
                </div>
              </Card>
            ))}
            {/* Agents live on the workspace roster, so adding one happens in the
                Workspace scope — but the chat view is the default, so surface a
                one-click entry point here that jumps there with the form open,
                instead of leaving "add an agent" undiscoverable behind the toggle. */}
            <button
              onClick={() => {
                setScope("workspace");
                setShowAddAgent(true);
              }}
              className="mt-1 flex w-full items-center justify-center gap-1.5 rounded-xl border border-dashed px-3 py-2 text-[12px] font-medium transition-colors hover:brightness-110"
              style={{ borderColor: "var(--ligus-line)", color: "var(--ligus-ink-soft)" }}
            >
              <IconPlus size={13} /> Add an agent
            </button>
          </Section>

          <Section title="Tools reachable">
            {enabledMcp.length === 0 ? (
              <EmptyHint text="No MCP servers are enabled, so no external tools are reachable here." />
            ) : (
              enabledMcp.map((server) => (
                <McpServerCard key={server.id} server={server} editable={false} />
              ))
            )}
          </Section>
        </>
      ) : (
        <>
          <Section
            title="Agent roster"
            action={
              <PaneHelpLink
                title="agents"
                about="Agents are personas you @mention, each bound to a host: this device, or a worker (a headless box you enroll under People → Remote agents)."
                docs="ligus/concepts/agents/"
              />
            }
          >
            {agentList.length === 0 ? (
              <EmptyHint text="No workspace agents yet. Add one below." />
            ) : (
              agentList.map((agent) => (
                <AgentRosterRow
                  key={agent.id}
                  sessionId={sessionId}
                  agent={agent}
                  hosts={hostList}
                  onRemove={() =>
                    void confirmDialog(agentRemovalConsequences(agent.name), {
                      title: `Remove @${agent.name}?`,
                      confirmLabel: "Remove agent",
                      danger: true,
                    }).then((ok) => ok && removeAgentMutation.mutate(agent.id))
                  }
                  onChanged={() => qc.invalidateQueries({ queryKey: ["agents", sessionId] })}
                />
              ))
            )}
            <FormDisclosure
              label="Add an agent"
              open={showAddAgent}
              onToggle={() => setShowAddAgent((v) => !v)}
            >
              <Card className="space-y-2 p-3">
                {(agentTemplates.data ?? []).length > 0 && (
                  <Field label="Start from a saved agent">
                    <SelectField
                      value=""
                      ariaLabel="Saved agent template"
                      onChange={(id) => {
                        const t = (agentTemplates.data ?? []).find((x) => x.id === id);
                        if (!t) return;
                        setAgentName(t.name);
                        setAgentRole(t.role ?? "");
                        if (t.runtimeId) setAgentRuntimeId(t.runtimeId);
                      }}
                    >
                      <option value="">Saved agents…</option>
                      {(agentTemplates.data ?? []).map((t) => (
                        <option key={t.id} value={t.id}>
                          {t.name}
                          {t.role ? ` · ${t.role}` : ""}
                        </option>
                      ))}
                    </SelectField>
                  </Field>
                )}
                <input
                  value={agentName}
                  onChange={(event) => setAgentName(event.target.value)}
                  placeholder="Agent name"
                  className="w-full rounded-xl border px-3 py-2 text-sm"
                  style={fieldStyle}
                />
                <input
                  value={agentRole}
                  onChange={(event) => setAgentRole(event.target.value)}
                  placeholder="Role (optional)"
                  className="w-full rounded-xl border px-3 py-2 text-sm"
                  style={fieldStyle}
                />
                <Field label="Agent runtime">
                  <SelectField value={agentRuntimeId} onChange={setAgentRuntimeId} ariaLabel="Agent runtime">
                    {runtimeList.map((runtime) => (
                      <option key={runtime.id} value={runtime.id}>
                        {runtimePickerLabel(runtime)}
                      </option>
                    ))}
                  </SelectField>
                </Field>
                <Field label="Runs on">
                  <SelectField value={agentHostId} onChange={setAgentHostId} ariaLabel="Agent host">
                    <option value="">This device</option>
                    {workerHosts.map((host) => (
                      <option key={host.id} value={host.id}>
                        {host.label || host.id}
                        {host.online ? "" : " (offline)"}
                      </option>
                    ))}
                  </SelectField>
                </Field>
                <Button
                  variant="primary"
                  disabled={!agentName.trim() || !agentRuntimeId || addAgentMutation.isPending}
                  onClick={() => addAgentMutation.mutate()}
                >
                  Add agent
                </Button>
              </Card>
            </FormDisclosure>
          </Section>

          <Section title="Runtimes">
            {runtimeList.length === 0 ? (
              <EmptyHint text="No runtimes configured yet." />
            ) : (
              runtimeList.map((rt) => (
                <RuntimeCard key={rt.id} runtime={rt} active={rt.id === activeRuntimeId} />
              ))
            )}
            <p className="px-1 pt-1 text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
              Manage in Settings → Models.
            </p>
          </Section>

          <Section title="MCP servers">
            {mcpList.length === 0 ? (
              <EmptyHint text="No MCP servers configured." />
            ) : (
              mcpList.map((server) => (
                <McpServerCard
                  key={server.id}
                  server={server}
                  editable
                  onToggle={(v) => toggleMcp(server, v)}
                  onRemove={
                    server.isManaged
                      ? () =>
                          confirmThen("Remove this MCP server?", () =>
                            removeMcpMutation.mutate(server.id),
                          )
                      : undefined
                  }
                />
              ))
            )}
            <FormDisclosure
              label="Install a server"
              open={showInstallMcp}
              onToggle={() => setShowInstallMcp((v) => !v)}
            >
              <Card className="space-y-2 p-3">
                <p className="text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                  Paste a manifest URL, GitHub blob URL, or owner/repo/path reference.
                </p>
                <input
                  value={mcpSource}
                  onChange={(event) => setMcpSource(event.target.value)}
                  placeholder="owner/repo/mcp.json or https://…"
                  className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
                  style={fieldStyle}
                />
                {mcpError && (
                  <div className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
                    {mcpError}
                  </div>
                )}
                <Button
                  variant="primary"
                  disabled={!mcpSource.trim() || installMcpMutation.isPending}
                  onClick={() => installMcpMutation.mutate()}
                >
                  Install
                </Button>
              </Card>
            </FormDisclosure>
          </Section>
        </>
      )}
    </RailFrame>
  );
}

/// The top-level scope split for the Tools pane (§6.3).
function ScopeToggle({ scope, onScope }: { scope: ToolsScope; onScope: (s: ToolsScope) => void }) {
  const opts: { id: ToolsScope; label: string }[] = [
    { id: "chat", label: "In this chat" },
    { id: "workspace", label: "Workspace" },
  ];
  return (
    <div
      className="mb-4 flex rounded-xl border p-0.5"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
    >
      {opts.map((o) => {
        const active = scope === o.id;
        return (
          <button
            key={o.id}
            onClick={() => onScope(o.id)}
            aria-pressed={active}
            className="ligus-press flex-1 rounded-lg px-3 py-1.5 text-[13px] font-medium"
            style={
              active
                ? {
                    background: "var(--ligus-panel)",
                    color: "var(--ligus-ink)",
                    boxShadow: "0 1px 2px color-mix(in srgb, var(--ligus-ink) 14%, transparent)",
                  }
                : { color: "var(--ligus-ink-soft)" }
            }
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/// One MCP server card (§6.4). Header carries the disclosure chevron, id,
/// transport detail, an enabled/inert label and a Switch (editable) or a plain
/// state pill (read-only). The backend exposes no per-tool policy or tool list
/// (McpServerDto = {id,transport,detail,enabled,isManaged}), so there are no
/// per-tool rows — only an honest note in the expanded body.
function McpServerCard({
  server,
  editable,
  onToggle,
  onRemove,
}: {
  server: McpServerDto;
  editable: boolean;
  onToggle?: (enabled: boolean) => void;
  onRemove?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const disabled = !server.enabled;
  return (
    <Card className="px-3 py-2.5">
      <div className="flex items-center gap-2">
        <button
          onClick={() => setOpen((o) => !o)}
          aria-label={open ? `Collapse ${server.id}` : `Expand ${server.id}`}
          data-tip={open ? "Collapse" : "Expand"}
          className="grid h-6 w-6 shrink-0 place-items-center rounded-md hover:bg-[color:var(--ligus-overlay)]"
          style={{ color: "var(--ligus-ink-soft)" }}
        >
          <span
            className="transition-transform"
            style={{ transform: open ? "rotate(0deg)" : "rotate(-90deg)" }}
          >
            <IconChevronDown size={14} />
          </span>
        </button>
        <div className="min-w-0 flex-1" style={{ opacity: disabled ? 0.6 : 1 }}>
          <div className="flex items-center gap-2">
            <span className="truncate text-[13px] font-medium">{server.id}</span>
            <span
              className="shrink-0 text-[11px] uppercase tracking-[0.08em]"
              style={{ color: disabled ? "var(--ligus-ink-soft)" : "var(--ligus-success-ink)" }}
            >
              {disabled ? "inert" : "enabled"}
            </span>
          </div>
          <div className="truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
            [{server.transport}] {server.detail}
          </div>
        </div>
        {editable ? (
          <Switch
            on={server.enabled}
            onChange={(v) => onToggle?.(v)}
            label={`Enable ${server.id}`}
          />
        ) : (
          <span
            className="shrink-0 rounded-full px-2 py-0.5 text-[11px] font-semibold uppercase"
            style={{ background: "var(--ligus-mist)", color: "var(--ligus-ink-soft)" }}
          >
            {disabled ? "off" : "on"}
          </span>
        )}
      </div>
      {open && (
        <div
          className="mt-2.5 border-t pt-2.5 text-[11.5px] leading-5"
          style={{ borderColor: "var(--ligus-line)", color: "var(--ligus-ink-soft)" }}
        >
          {disabled
            ? "Disabled — inert gate: this server is never launched or connected."
            : "Per-tool permissions arrive with backend support."}
          {editable && onRemove && (
            <div className="mt-2.5">
              <Button variant="danger" onClick={onRemove}>
                Remove server
              </Button>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

/// Host badge shown on every agent card. Resolves an agent's `hostId` against
/// the workspace host roster: empty ⇒ the owner's own device (a local persona),
/// a worker id ⇒ a headless, worker-hosted agent. Always visible so the two
/// kinds are never confused.
function AgentHostBadge({ hostId, hosts }: { hostId: string; hosts: WorkspaceHostDto[] }) {
  if (!hostId) {
    return (
      <span
        className="inline-flex items-center gap-1 text-[11px]"
        style={{ color: "var(--ligus-ink-soft)" }}
        data-tip="Runs on this device — a local persona"
      >
        <span className="h-2 w-2 rounded-full" style={{ background: "var(--ligus-line)" }} aria-hidden />
        This device
      </span>
    );
  }
  const host = hosts.find((h) => h.id === hostId);
  const label = host?.label || `${hostId.slice(0, 8)}…`;
  const online = host?.online ?? false;
  return (
    <span
      className="inline-flex items-center gap-1 text-[11px]"
      style={{ color: "var(--ligus-ink-soft)" }}
      data-tip={
        online
          ? `Headless — runs on worker “${label}”`
          : `Worker “${label}” is offline; queued work waits until it reconnects`
      }
    >
      <span aria-hidden className="inline-flex"><IconMonitor size={12} /></span>
      <span className="truncate">{label}</span>
      <span aria-hidden>·</span>
      <span style={{ color: online ? "var(--ligus-success-ink)" : "var(--ligus-warn-ink)" }}>
        {online ? "online" : "offline"}
      </span>
    </span>
  );
}

/// What removing a workspace agent costs, said before it happens. The roster
/// is workspace-wide and synced, so it's gone for everyone; workflow stages
/// that name it fail ("its agent is no longer in the roster").
export function agentRemovalConsequences(name: string): string {
  return (
    `@${name} leaves every chat in this workspace, for everyone. ` +
    `Mentions of @${name} go unanswered, and workflow stages that use it stop with an error. ` +
    `Its avatar and access settings are deleted; adding it again starts from scratch.`
  );
}

/// Editable agent row (Workspace scope): avatar upload + host badge + remove.
function AgentRosterRow({
  sessionId,
  agent,
  hosts,
  onRemove,
  onChanged,
}: {
  sessionId: string;
  agent: import("@/lib/ipc").WorkspaceAgentDto;
  hosts: WorkspaceHostDto[];
  onRemove: () => void;
  onChanged: () => void;
}) {
  return (
    <Card className="px-3 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-2.5">
          <label className="group/av relative cursor-pointer" data-tip="Upload an avatar for this agent">
            <Avatar
              name={agent.name}
              url={agent.avatarUrl}
              colorHex={agent.avatarColorHex}
              kind="agent"
              size={32}
            />
            <span
              className="absolute inset-0 flex items-center justify-center rounded-[30%] text-[11px] font-semibold opacity-0 transition-opacity group-hover/av:opacity-100"
              style={{
                background: "color-mix(in srgb, var(--ligus-ink) 55%, transparent)",
                color: "var(--ligus-canvas)",
              }}
            >
              Edit
            </span>
            <input
              type="file"
              accept="image/*"
              className="hidden"
              onChange={async (e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (!file) return;
                try {
                  const url = await fileToAvatarDataUrl(file);
                  await setAgentAvatar(sessionId, agent.id, url, null);
                  onChanged();
                  toast.success("Agent avatar updated.");
                } catch (err) {
                  toast.error(`Couldn't set avatar: ${errMsg(err)}`);
                }
              }}
            />
          </label>
          <div className="min-w-0">
            <div className="font-medium">@{agent.name}</div>
            <div className="mt-1 text-xs text-ink-soft">
              {agent.runtimeId} · {agent.role || "agent"}
            </div>
            <div className="mt-1">
              <AgentHostBadge hostId={agent.hostId} hosts={hosts} />
            </div>
            {!agent.canManageAccess && (
              <div className="mt-1">
                <AgentAccessPanel sessionId={sessionId} agent={agent} />
              </div>
            )}
          </div>
        </div>
        <MoreMenu
          label={`More actions for @${agent.name}`}
          items={[
            ...(agent.avatarUrl
              ? [
                  {
                    label: "Clear avatar",
                    onSelect: async () => {
                      try {
                        await setAgentAvatar(sessionId, agent.id, null, null);
                        onChanged();
                      } catch (err) {
                        toast.error(`Couldn't clear avatar: ${errMsg(err)}`);
                      }
                    },
                  },
                ]
              : []),
            { label: "Remove from workspace…", danger: true, onSelect: onRemove },
          ]}
        />
      </div>
      {agent.canManageAccess && <AgentAccessPanel sessionId={sessionId} agent={agent} />}
    </Card>
  );
}

function RuntimeCard({
  runtime,
  active,
  onSelect,
}: {
  runtime: RuntimeSummaryDto;
  active: boolean;
  onSelect?: () => void;
}) {
  const style = {
    borderColor: active ? "var(--ligus-accent-cool)" : "var(--ligus-line)",
    background: active ? SELECT_TINT : "var(--ligus-mist)",
  } as const;
  const inner = (
    <div className="flex items-center justify-between gap-3">
      <div className="min-w-0">
        <div className="truncate text-base font-semibold">{runtime.label}</div>
        <div className="mt-1 truncate text-xs text-ink-soft">
          {runtime.location} · {runtime.provider}
          {runtime.endpoint ? ` · ${runtime.endpoint}` : ""}
        </div>
      </div>
      <div className="flex items-center gap-2">
        {runtime.isManaged && (
          <span
            className="rounded-full px-2 py-1 text-[11px] font-semibold uppercase text-ink-soft"
            style={{ background: "var(--ligus-mist)" }}
            data-tip="A runtime you configured (vs. an auto-detected local CLI)"
          >
            Configured
          </span>
        )}
        {active && (
          <span
            className="rounded-full px-2 py-1 text-xs font-semibold"
            style={{
              background: "color-mix(in srgb, var(--ligus-success) 18%, transparent)",
              color: "var(--ligus-success-ink)",
            }}
          >
            Active
          </span>
        )}
      </div>
    </div>
  );
  if (!onSelect) {
    return (
      <div className="w-full rounded-2xl border px-3.5 py-3.5 text-left" style={style}>
        {inner}
      </div>
    );
  }
  return (
    <button
      onClick={onSelect}
      className="w-full rounded-2xl border px-3.5 py-3.5 text-left transition-colors hover:bg-[color:var(--ligus-overlay)]"
      style={style}
    >
      {inner}
    </button>
  );
}

// ─── Queued work (spec §12.5) ────────────────────────────────────────────────
// Unanswered agent mentions in the workspace, tagged with each agent's host
// status. The desktop counterpart of the CLI's `ligus queue`: an agent bound to
// an offline host (or a member's device) has its mentions *queued* until the
// host returns — so we surface a one-click "run on worker" handoff
// (`ligus set-agent-host`) to move it to an always-on worker instead.

/// Status pill for a queued item's host: online = success, offline = warn
/// ("OFFLINE → queued"), device/unknown = neutral. All tints route through
/// --ligus-* tokens (R1).
function HostStatusBadge({ item }: { item: QueuedWorkDto }) {
  switch (item.hostStatus) {
    case "online":
      return <Badge tone="success">{item.hostLabel ? `online · ${item.hostLabel}` : "online"}</Badge>;
    case "offline":
      return (
        <Badge tone="warn" title={item.hostLabel ? `${item.hostLabel} is offline` : undefined}>
          offline · queued
        </Badge>
      );
    case "device":
      return <Badge>device</Badge>;
    default:
      return <Badge>unknown host</Badge>;
  }
}

/// "Run on worker" affordance: a small menu of the online workers. Selecting
/// one reassigns the agent's host and refreshes the queue + roster.
function RunOnWorkerMenu({
  workers,
  onReassign,
}: {
  workers: WorkspaceHostDto[];
  onReassign: (hostId: string) => void;
}) {
  const anchorRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        ref={anchorRef}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="shrink-0 rounded-lg border px-2 py-1 text-[11.5px] font-medium transition-colors hover:bg-[color:var(--ligus-overlay)]"
        style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
      >
        Run on worker
      </button>
      <Menu
        open={open}
        anchorRef={anchorRef}
        onClose={() => setOpen(false)}
        align="right"
        minWidth={200}
        label="Online workers"
        items={[
          { type: "header", label: "Online workers" },
          ...(workers.length === 0
            ? [{ label: "No online workers", onClick: () => {}, disabled: true }]
            : workers.map((w) => ({
                label: hostGatewaysLabel(w) ? `${w.label || w.id} · gateways: ${hostGatewaysLabel(w)}` : w.label || w.id,
                onClick: () => onReassign(w.id),
              }))),
        ]}
      />
    </>
  );
}

function QueuedWorkRow({
  item,
  workers,
  onReassign,
}: {
  item: QueuedWorkDto;
  workers: WorkspaceHostDto[];
  onReassign: (agentId: string, hostId: string) => void;
}) {
  const needsHandoff = item.hostStatus === "offline" || item.hostStatus === "device";
  return (
    <Card className="px-3 py-2.5">
      <div className="flex min-w-0 items-baseline gap-1.5">
        <span className="shrink-0 text-[13px] font-semibold">@{item.agentName}</span>
        <span className="min-w-0 truncate text-xs text-ink-soft">in {item.chatTitle || "Untitled chat"}</span>
      </div>
      <div className="mt-1.5 flex flex-wrap items-center justify-between gap-2">
        <HostStatusBadge item={item} />
        {needsHandoff && (
          <RunOnWorkerMenu
            workers={workers}
            onReassign={(hostId) => onReassign(item.agentId, hostId)}
          />
        )}
      </div>
    </Card>
  );
}

/// The "Queued work" section: unanswered agent mentions across the workspace,
/// each with its host status + a "run on worker" handoff for offline/device
/// agents.
function QueuedWorkSection() {
  const qc = useQueryClient();
  const queued = useQuery({ queryKey: ["queued-work"], queryFn: listQueuedWork });
  const hosts = useQuery({ queryKey: ["workspace-hosts"], queryFn: listWorkspaceHosts });
  const workers = (hosts.data ?? []).filter((h) => h.kind === "worker" && h.online);

  const reassign = useMutation({
    mutationFn: (v: { agentId: string; hostId: string }) => setAgentHost(v.agentId, v.hostId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["queued-work"] });
      qc.invalidateQueries({ queryKey: ["agents"] });
      toast.success("Moved to worker — it'll run there.");
    },
    onError: (e) => toast.error(`Couldn't reassign: ${errMsg(e)}`),
  });

  const items = queued.data ?? [];
  return (
    <Section title="Queued work">
      {items.length === 0 ? (
        <EmptyHint text="Nothing queued." />
      ) : (
        items.map((item) => (
          <QueuedWorkRow
            key={`${item.sessionId}:${item.agentId}`}
            item={item}
            workers={workers}
            onReassign={(agentId, hostId) => reassign.mutate({ agentId, hostId })}
          />
        ))
      )}
    </Section>
  );
}

/// One proposal in the Review inbox.
///
/// `settled` proposals keep their record and their diff, but lose the vote
/// buttons — a proposal that's already applied or rejected can't be voted on, so
/// offering Approve/Reject there only invites a click that does nothing.
/// Per-file, collapsible unified-diff view for a proposal — closer to a PR
/// review than one flat block: each file has its own header (+adds/−removes) and
/// its own horizontal scroll, with line numbers on the new side.
function ProposalDiff({ diff, wide = false }: { diff: string; wide?: boolean }) {
  const files = parseUnifiedDiff(diff);
  return (
    <div className={wide ? "space-y-3" : "mt-3 space-y-2"}>
      {files.map((f, fi) => (
        <DiffFileBlock
          key={`${f.path}-${fi}`}
          file={f}
          // In the wide (modal) view every file opens — you came to read them.
          defaultOpen={wide || files.length <= 2}
          wide={wide}
        />
      ))}
    </div>
  );
}

function DiffFileBlock({
  file,
  defaultOpen,
  wide = false,
}: {
  file: ReturnType<typeof parseUnifiedDiff>[number];
  defaultOpen: boolean;
  wide?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  // Roomier type in the full-diff modal, compact in the narrow pane.
  const fontClass = wide ? "text-[13px] leading-[1.6]" : "text-[11px] leading-[1.5]";
  const pathClass = wide ? "text-[13px]" : "text-[11px]";
  let newLine = 0;
  return (
    <div className="overflow-hidden rounded-lg border" style={{ borderColor: "var(--ligus-line)" }}>
      <button
        onClick={() => setOpen((o) => !o)}
        className="ligus-press flex w-full items-center gap-2 px-2.5 py-1.5 text-left"
        style={{ background: "var(--ligus-mist)" }}
      >
        <span className="text-ink-soft" aria-hidden>{open ? "▾" : "▸"}</span>
        <span className={`min-w-0 flex-1 truncate font-mono ${pathClass}`}>{file.path}</span>
        {file.added > 0 && <span className={pathClass} style={{ color: "var(--ligus-success-ink)" }}>+{file.added}</span>}
        {file.removed > 0 && <span className={pathClass} style={{ color: "var(--ligus-danger-ink)" }}>−{file.removed}</span>}
      </button>
      {open && (
        <div className={`overflow-x-auto font-mono ${fontClass}`} style={{ background: "var(--ligus-overlay)" }}>
          {file.lines.map((line, i) => {
            const isAdd = line.startsWith("+") && !line.startsWith("+++");
            const isDel = line.startsWith("-") && !line.startsWith("---");
            const isHunk = line.startsWith("@@");
            const isMeta = line.startsWith("diff ") || line.startsWith("index ") || line.startsWith("+++") || line.startsWith("---");
            if (isHunk) {
              const m = /\+(\d+)/.exec(line);
              if (m) newLine = parseInt(m[1], 10);
            }
            const num = isDel || isMeta || isHunk ? "" : String(newLine++);
            const color = isAdd
              ? "var(--ligus-success-ink)"
              : isDel
                ? "var(--ligus-danger-ink)"
                : isHunk
                  ? "var(--ligus-accent-ink)"
                  : isMeta
                    ? "var(--ligus-ink-soft)"
                    : "var(--ligus-ink)";
            const rowBg = isAdd ? "var(--ligus-success-tint)" : isDel ? "var(--ligus-danger-tint)" : isHunk ? "var(--ligus-info-tint)" : undefined;
            return (
              <div key={i} className="flex" style={{ whiteSpace: "pre", background: rowBg }}>
                <span
                  className="select-none pr-2 pl-2 text-right"
                  style={{ minWidth: "3ch", color: "var(--ligus-ink-faint)" }}
                  aria-hidden
                >
                  {num}
                </span>
                <span style={{ color }}>{line || " "}</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export type ApplyConflictKind = "conflict" | "dirty";

/// Which apply failure on a repo chat's branch this is (the backend's
/// "Not applied: …" sentences): a patch that conflicts with what landed
/// since (rebase it, or look at it), or local changes in the chat's folder
/// that are in the way (look at them). Anything else is a plain error.
export function applyConflictKind(message: string): ApplyConflictKind | null {
  if (!message.startsWith("Not applied")) return null;
  if (message.includes("doesn't apply on this chat's branch")) return "conflict";
  if (message.includes("local changes in the way")) return "dirty";
  return null;
}

/// "Ask the agent to rebase" files a follow-up turn with the conflict;
/// "Open in editor" opens the chat's folder for its repository.
export function ApplyConflictActions({
  conflict,
  busy,
  rebasing,
  onRebase,
  onOpen,
}: {
  conflict: { kind: ApplyConflictKind; message: string };
  busy: boolean;
  rebasing: boolean;
  onRebase: () => void;
  onOpen: () => void;
}) {
  return (
    <div role="alert" className="mt-2 rounded-lg border px-3 py-2 text-xs" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="mb-1.5">{conflict.message}</div>
      <div className="flex flex-wrap gap-2">
        {conflict.kind === "conflict" && (
          <Button size="sm" variant="primary" disabled={busy} onClick={onRebase}>
            {rebasing ? "Asking…" : "Ask the agent to rebase"}
          </Button>
        )}
        <Button size="sm" disabled={busy} onClick={onOpen}>
          Open in editor
        </Button>
      </div>
    </div>
  );
}

function ProposalCard({
  proposal,
  sessionId,
  onChanged,
  onDismiss,
}: {
  proposal: ProposalDto;
  sessionId: string;
  onChanged: () => void;
  onDismiss: (dismissed: boolean) => void;
}) {
  const qc = useQueryClient();
  const settled = isSettled(proposal);
  // Bulky detail (body + diff) collapses; active proposals open by default (you're
  // reviewing them), settled ones start collapsed (they're history).
  const [expanded, setExpanded] = useState(!settled);
  const hasDetail = Boolean(proposal.body) || Boolean(proposal.diff) || Boolean(proposal.workflow);
  // Who "you" are, to show your own vote (shares the People pane's cache).
  const members = useQuery({ queryKey: ["members", sessionId], queryFn: () => listMembers(sessionId) });
  const selfActorId = members.data?.find((m) => m.isSelf)?.actorId;
  const mine = myVote(proposal, selfActorId);
  // A workflow proposal renders its stages; they name agents by id.
  const agents = useQuery({
    queryKey: ["agents", sessionId],
    queryFn: () => listAgents(sessionId),
    enabled: Boolean(proposal.workflow),
  });

  // Write the freshly-projected proposal straight into the cache so the card
  // reflects the new status/approvals the instant the vote returns — the old code
  // discarded this DTO and waited on a refetch, which is why the buttons "didn't
  // update after selection".
  const applyUpdated = (updated: ProposalDto | null) => {
    if (!updated) return;
    qc.setQueryData<ProposalDto[]>(["proposals", sessionId], (prev) =>
      prev ? prev.map((p) => (p.id === updated.id ? updated : p)) : prev,
    );
  };

  const vote = useMutation({
    mutationFn: (approved: boolean) => voteProposal(sessionId, proposal.id, approved),
    onSuccess: (updated) => {
      applyUpdated(updated);
      onChanged();
    },
    onError: (e) => toast.error(`Couldn't record your vote: ${errMsg(e)}`),
  });

  // An apply that didn't land on the chat's branch: what to do next.
  const [applyConflict, setApplyConflict] = useState<{ kind: ApplyConflictKind; message: string } | null>(null);
  const implement = useMutation({
    mutationFn: (target: string | null) => implementProposal(sessionId, proposal.id, target),
    onSuccess: () => {
      setApplyConflict(null);
      onChanged();
      toast.success(proposal.workflow ? "Workflow added." : proposal.argv ? "Command ran." : "Sent to the agent to implement.");
    },
    onError: (e) => {
      const kind = applyConflictKind(errMsg(e));
      if (kind) setApplyConflict({ kind, message: errMsg(e) });
      else toast.error(`Couldn't implement: ${errMsg(e)}`);
    },
  });
  const rebase = useMutation({
    mutationFn: () => askAgentToRebase(sessionId, proposal.id),
    onSuccess: () => {
      setApplyConflict(null);
      onChanged();
      toast.success("Asked the agent to rebase it.");
    },
    onError: (e) => toast.error(`Couldn't ask the agent: ${errMsg(e)}`),
  });
  const openEditor = useMutation({
    mutationFn: () => openChatCheckout(sessionId),
    onError: (e) => toast.error(`Couldn't open it: ${errMsg(e)}`),
  });
  // Apply on a host (Phase B): one named host applies it, once.
  const openRequest = proposal.applyRequests?.find((r) => r.state === "open");
  const hosts = useQuery({
    queryKey: ["workspace-hosts"],
    queryFn: listWorkspaceHosts,
    enabled: Boolean(proposal.canRequestApply),
  });
  const applyHosts = (hosts.data ?? []).filter((h) => UUID_RE.test(h.id));
  const [applyHost, setApplyHost] = useState<string>("");
  const pickedHost = applyHosts.find((h) => h.id === applyHost) ?? applyHosts[0];
  const requestApply = useMutation({
    mutationFn: (hostId: string) => requestApplyOnHost(sessionId, proposal.id, hostId),
    onSuccess: () => {
      toast.success(`Asked ${pickedHost?.label ?? "the host"} to apply it.`);
      onChanged();
    },
    onError: (e) => toast.error(`Couldn't ask the host: ${errMsg(e)}`),
  });
  const withdraw = useMutation({
    mutationFn: (requestId: string) => withdrawApplyRequest(sessionId, requestId),
    onSuccess: () => {
      toast.success("Withdrawn.");
      onChanged();
    },
    onError: (e) => toast.error(`Couldn't withdraw: ${errMsg(e)}`),
  });
  const hostName = (r: ApplyRequestDto) => r.hostLabel ?? r.hostDeviceId.slice(0, 8);

  // Implement shows exactly what will run first (Track D4).
  const [previewOpen, setPreviewOpen] = useState(false);

  // Full-width diff viewer — the per-file diff is cramped in this narrow pane.
  const [diffOpen, setDiffOpen] = useState(false);

  // Refinement thread: a note draft + whether it's a change request (vs. comment).
  const [note, setNote] = useState("");
  const [asChangeRequest, setAsChangeRequest] = useState(false);
  const refine = useMutation({
    mutationFn: () => refineProposal(sessionId, proposal.id, note.trim(), asChangeRequest),
    onSuccess: (updated) => {
      applyUpdated(updated);
      setNote("");
      setAsChangeRequest(false);
      onChanged();
    },
    onError: (e) => toast.error(`Couldn't add your note: ${errMsg(e)}`),
  });
  const sendBack = useMutation({
    mutationFn: () => requestProposalChanges(sessionId, proposal.id),
    onSuccess: () => {
      onChanged();
      toast.success("Sent back to the agent to revise.");
    },
    onError: (e) => toast.error(`Couldn't send it back: ${errMsg(e)}`),
  });

  const superseded = Boolean(proposal.supersededBy);
  const hasChangeRequests = proposal.refinements.some((r) => r.requestChanges);

  // Any in-flight action disables the others so a card can't be double-driven.
  const busy =
    vote.isPending ||
    implement.isPending ||
    rebase.isPending ||
    refine.isPending ||
    sendBack.isPending ||
    requestApply.isPending ||
    withdraw.isPending;

  return (
    <Card className="px-4 py-4">
      <div className="flex items-start justify-between gap-3">
        <button
          type="button"
          className="-mx-1 -my-0.5 flex min-w-0 items-start gap-1.5 rounded-md px-1 py-0.5 text-left transition-colors enabled:hover:bg-[color:var(--ligus-hover)]"
          onClick={() => hasDetail && setExpanded((v) => !v)}
          aria-expanded={hasDetail ? expanded : undefined}
          disabled={!hasDetail}
        >
          {hasDetail && (
            <span
              aria-hidden
              className="mt-0.5 shrink-0 transition-transform"
              style={{ transform: expanded ? "rotate(0deg)" : "rotate(-90deg)", opacity: 0.5 }}
            >
              <IconChevronDown size={14} />
            </span>
          )}
          <span className="min-w-0 font-semibold">{proposal.title}</span>
        </button>
        <div className="flex shrink-0 items-center gap-2">
          <StatusPill status={proposal.status} />
          {settled && (
            <IconButton
              label={proposal.dismissed ? "Restore to the inbox" : "Dismiss"}
              onClick={() => onDismiss(!proposal.dismissed)}
            >
              <IconX size={12} />
            </IconButton>
          )}
        </div>
      </div>
      <div className="mt-0.5 pl-[22px] text-xs text-ink-soft">{relTime(proposal.createdAt)}</div>
      <ProposalBadges proposal={proposal} />

      {superseded && (
        <div
          className="mt-2 rounded-lg px-2.5 py-1.5 text-xs"
          style={{ background: "var(--ligus-mist)", color: "var(--ligus-ink-soft)" }}
        >
          Superseded by a newer revision.
        </div>
      )}

      {expanded && (
        <>
          {proposal.workflow ? (
            <>
              {proposal.workflow.description && (
                <Markdown content={proposal.workflow.description} size="sm" className="mt-2 text-ink-soft" />
              )}
              <div className="mt-2 text-xs text-ink-soft">
                Approving adds this workflow to the Workflows pane. Runs still start only when someone presses Run.
              </div>
              <WorkflowStages
                nodes={proposal.workflow.nodes}
                agentName={(id) => agents.data?.find((a) => a.id === id)?.name}
              />
            </>
          ) : (
            proposal.body && (
              <div data-testid="proposal-body">
                <Markdown content={proposal.body} size="sm" className="mt-2 text-ink-soft" />
              </div>
            )
          )}
          {/* Issue #118: an agent's file offer. The reviewer sees exactly
              what approving shares (its content was checked when offered; the
              host uploads only this file, by hash). */}
          {proposal.offeredFile && (
            <div
              data-testid="offered-file"
              className="mt-2 rounded-md border px-2 py-1.5 text-xs"
              style={{ borderColor: proposal.offeredFile.risky ? "var(--ligus-danger)" : "var(--ligus-line)" }}
            >
              <div className="font-medium">Shares the file “{proposal.offeredFile.name}” with this chat</div>
              <div className="text-ink-soft">
                {proposal.offeredFile.contentType} · {proposal.offeredFile.size} bytes · SHA-256 {proposal.offeredFile.sha256.slice(0, 16)}…
              </div>
              {proposal.offeredFile.risky && <div role="alert">Its type can run code when opened.</div>}
            </div>
          )}
          {/* A fileDiff proposal (from an agent's isolated worktree) carries a
              unified diff. It's cramped in this narrow pane, so offer opening it
              full-width in the Diff canvas; the inline per-file view stays as a
              quick glance. */}
          {proposal.diff && (
            <>
              <div className="mt-2 flex items-center justify-between gap-2">
                <span className="text-xs text-ink-soft">
                  {(proposal.changedFiles?.length ?? 0)} file
                  {(proposal.changedFiles?.length ?? 0) === 1 ? "" : "s"} changed
                </span>
                <button
                  type="button"
                  className="ligus-press rounded-md px-2 py-0.5 text-xs font-medium hover:border-[color:var(--tint-cool-line)]"
                  style={{ border: "1px solid var(--ligus-line)", color: "var(--ligus-accent-ink)" }}
                  onClick={() => setDiffOpen(true)}
                >
                  View full diff ↗
                </button>
              </div>
              <ProposalDiff diff={proposal.diff} />
            </>
          )}

          {/* Refinement thread — comments + change requests from humans and
              role-qualified agents. */}
          <RefinementThread
            proposal={proposal}
            note={note}
            setNote={setNote}
            asChangeRequest={asChangeRequest}
            setAsChangeRequest={setAsChangeRequest}
            onSubmit={() => note.trim() && refine.mutate()}
            submitting={refine.isPending}
            disabled={busy || superseded}
          />
        </>
      )}

      <div className="mt-3 text-xs text-ink-soft">
        {proposal.qualifyingApprovals}/{proposal.requiredApprovals} approvals
      </div>
      {/* Who voted: tag each approver/rejecter by name + role. */}
      {proposal.approvals.length > 0 && (
        <div className="mt-1.5 flex flex-col gap-1">
          {proposal.approvals.map((a) => (
            <div key={a.actorId} className="flex items-center gap-1.5 text-xs">
              <span
                aria-hidden
                style={{ color: a.approved ? "var(--ligus-success-ink)" : "var(--ligus-danger-ink)" }}
              >
                {a.approved ? "✓" : "✕"}
              </span>
              <span className="font-medium">{a.displayName}</span>
              <span className="text-ink-soft">
                {a.approved ? "approved" : "rejected"} · {a.role}
              </span>
            </div>
          ))}
        </div>
      )}

      {/* Your own vote, once cast: said plainly, with the other choice offered as
          a change (votes are latest-per-person, so changing one is allowed). */}
      {mine !== null && isVotable(proposal) && (
        <div
          data-testid="my-vote"
          className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs"
        >
          <span
            className="inline-flex items-center gap-1 font-medium"
            style={{ color: mine ? "var(--ligus-success-ink)" : "var(--ligus-danger-ink)" }}
          >
            <span aria-hidden>{mine ? "✓" : "✕"}</span>
            {mine ? "You approved" : "You rejected"}
          </span>
          <LinkButton disabled={busy} onClick={() => vote.mutate(!mine)}>
            {vote.isPending ? "Changing…" : mine ? "Change to reject" : "Change to approve"}
          </LinkButton>
        </div>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        {/* Vote buttons only while you haven't voted; once you have, the line
            above holds your vote and its change. When quorum is already met
            the vote is secondary, so it goes quiet beside Implement. */}
        {isVotable(proposal) && mine === null && (
          <>
            <Button
              size="md"
              variant={proposal.quorumMet ? "quiet" : "success"}
              disabled={busy}
              onClick={() => vote.mutate(true)}
            >
              {vote.isPending && vote.variables === true ? "Approving…" : "Approve"}
            </Button>
            <Button
              size="md"
              variant={proposal.quorumMet ? "quiet" : "danger"}
              disabled={busy}
              onClick={() => vote.mutate(false)}
            >
              {vote.isPending && vote.variables === false ? "Rejecting…" : "Reject"}
            </Button>
          </>
        )}
        {/* Agreement gate: an approved proposal only runs when a human explicitly
            implements it; the agent then carries it out. */}
        {proposal.quorumMet && proposal.status !== "applied" && (
          <Button
            variant="primary"
            size="md"
            className="ml-auto"
            disabled={busy || Boolean(openRequest)}
            title={
              openRequest
                ? `${hostName(openRequest)} was asked to apply it. One applier per proposal: withdraw that request to implement it here.`
                : undefined
            }
            onClick={() => setPreviewOpen(true)}
          >
            {implement.isPending ? "Implementing…" : "Implement…"}
          </Button>
        )}
        {proposal.status === "applied" && (
          <span className="ml-auto self-center text-xs text-ink-soft">Implemented</span>
        )}
      </div>
      {proposal.quorumMet && proposal.status !== "applied" && !openRequest && (
        <div data-testid="next-step" className="mt-1.5 text-xs text-ink-soft">
          Quorum met. Next: implement it{proposal.canRequestApply && applyHosts.length > 0 ? ", or ask a host to apply it" : ""}.
        </div>
      )}

      {applyConflict && proposal.status !== "applied" && (
        <ApplyConflictActions
          conflict={applyConflict}
          busy={busy}
          onRebase={() => rebase.mutate()}
          onOpen={() => openEditor.mutate()}
          rebasing={rebase.isPending}
        />
      )}

      {/* Apply on a host: the requests a person filed, and how each ended. */}
      {(proposal.applyRequests?.length ?? 0) > 0 && (
        <div className="mt-2 flex flex-col gap-1 text-xs">
          {proposal.applyRequests!.map((r) => (
            <div key={r.id} className="flex items-center gap-2 text-ink-soft">
              {r.state === "open" && <span>Waiting for {hostName(r)} to apply it</span>}
              {r.state === "applied" && (
                <span style={{ color: "var(--ligus-success-ink)" }}>
                  Applied on {hostName(r)}
                  {r.commit ? ` (${r.commit.slice(0, 12)})` : ""}
                </span>
              )}
              {r.state === "failed" && (
                <span style={{ color: "var(--ligus-danger-ink)" }}>
                  {hostName(r)} couldn't apply it: {r.reason ?? "no reason given"}
                </span>
              )}
              {r.state === "withdrawn" && <span>Withdrawn</span>}
              {r.state === "open" && r.mine && (
                <LinkButton disabled={busy} onClick={() => withdraw.mutate(r.id)}>
                  Withdraw
                </LinkButton>
              )}
            </div>
          ))}
        </div>
      )}
      {proposal.canRequestApply && applyHosts.length > 0 && (
        <div className="mt-2 flex items-center gap-2">
          {applyHosts.length > 1 && (
            <select
              aria-label="Host to apply on"
              className="min-w-0 rounded-lg border bg-transparent px-2 py-1 text-xs"
              style={{ borderColor: "var(--ligus-line)" }}
              value={pickedHost?.id ?? ""}
              onChange={(e) => setApplyHost(e.target.value)}
            >
              {applyHosts.map((h) => (
                <option key={h.id} value={h.id}>
                  {h.label} {h.online ? "(online)" : "(offline)"}
                </option>
              ))}
            </select>
          )}
          <Button
            variant="ghost"
            disabled={busy || !pickedHost}
            title="That host applies it once, on its checkout, and reports the result. Nobody else applies it meanwhile."
            onClick={() => pickedHost && requestApply.mutate(pickedHost.id)}
          >
            {requestApply.isPending ? "Asking…" : `Apply on ${pickedHost?.label ?? "host"}`}
          </Button>
          {applyHosts.length === 1 && (
            <span className="text-xs text-ink-soft">{pickedHost?.online ? "online" : "offline"}</span>
          )}
        </div>
      )}

      {/* Send-back-to-agent: available while the proposal is still live and there's
          feedback to act on. The agent produces a linked new version. */}
      {!superseded && proposal.status !== "applied" && hasChangeRequests && (
        <Button
          size="md"
          disabled={busy}
          className="mt-2 w-full border-dashed"
          onClick={() => sendBack.mutate()}
        >
          {sendBack.isPending ? "Sending to agent…" : "Send to agent to revise"}
        </Button>
      )}

      {previewOpen && (
        <ImplementPreview
          sessionId={sessionId}
          proposalId={proposal.id}
          title={proposal.title}
          onClose={() => setPreviewOpen(false)}
          onConfirm={(target) => {
            setPreviewOpen(false);
            implement.mutate(target);
          }}
        />
      )}
      {diffOpen && proposal.diff && (
        <ProposalDiffModal
          title={proposal.title}
          diff={proposal.diff}
          files={proposal.changedFiles?.length ?? 0}
          onClose={() => setDiffOpen(false)}
        />
      )}
    </Card>
  );
}

/// A proposal's full diff in a wide sheet (Review card, proposal-set
/// alternatives).
export function ProposalDiffModal({
  title,
  diff,
  files,
  onClose,
}: {
  title: string;
  diff: string;
  files: number;
  onClose: () => void;
}) {
  return (
    <Modal
      onClose={onClose}
      panelClassName="flex max-h-[88vh] w-[min(96vw,1100px)] flex-col overflow-hidden rounded-2xl border"
      panelStyle={{ background: "var(--ligus-panel)", borderColor: "var(--ligus-line)", boxShadow: "var(--ligus-shadow-pop)" }}
    >
      <div className="flex items-center justify-between gap-3 border-b px-4 py-3" style={{ borderColor: "var(--ligus-line)" }}>
        <div className="min-w-0">
          <div className="truncate font-semibold">{title}</div>
          <div className="text-xs text-ink-soft">
            {files} file{files === 1 ? "" : "s"} changed
          </div>
        </div>
        <IconButton label="Close" onClick={onClose}>
          <IconX size={14} />
        </IconButton>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-4">
        <ProposalDiff diff={diff} wide />
      </div>
    </Modal>
  );
}

/// The refinement discussion under a proposal: existing notes (tagged by author +
/// role, change requests flagged), and an input to add a comment or change request.
function RefinementThread({
  proposal,
  note,
  setNote,
  asChangeRequest,
  setAsChangeRequest,
  onSubmit,
  submitting,
  disabled,
}: {
  proposal: ProposalDto;
  note: string;
  setNote: (v: string) => void;
  asChangeRequest: boolean;
  setAsChangeRequest: (v: boolean) => void;
  onSubmit: () => void;
  submitting: boolean;
  disabled: boolean;
}) {
  return (
    <div className="mt-3 border-t pt-3" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="text-xs font-medium text-ink-soft">Refinement</div>
      {proposal.refinements.length > 0 && (
        <div className="mt-2 flex flex-col gap-2">
          {proposal.refinements.map((r) => (
            <div key={r.id} className="text-xs">
              <div className="flex items-center gap-1.5">
                <span className="font-medium">{r.displayName}</span>
                <span className="text-ink-soft">{r.role}</span>
                {r.requestChanges && (
                  <span
                    className="rounded-full px-1.5 py-0.5 text-[11px] font-semibold uppercase tracking-wide"
                    style={{
                      color: "var(--ligus-warm-ink)",
                      background: "color-mix(in srgb, var(--ligus-accent-warm) 15%, transparent)",
                    }}
                  >
                    changes
                  </span>
                )}
                <span className="text-ink-soft">· {relTime(r.createdAt)}</span>
              </div>
              <div data-testid="refinement-text" className="mt-0.5 break-words text-ink-soft">
                <Markdown content={r.text} size="xs" />
              </div>
            </div>
          ))}
        </div>
      )}
      <div className="mt-2 flex flex-col gap-1.5">
        <textarea
          value={note}
          onChange={(e) => setNote(e.target.value)}
          placeholder="Comment or request a change…"
          rows={2}
          disabled={disabled}
          className="w-full resize-y rounded-xl border px-3 py-2 text-sm outline-none focus:border-[color:var(--ligus-accent-cool)] disabled:opacity-50"
          style={fieldStyle}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") onSubmit();
          }}
        />
        <div className="flex items-center justify-between gap-2">
          <label className="flex items-center gap-1.5 text-xs text-ink-soft">
            <input
              type="checkbox"
              checked={asChangeRequest}
              onChange={(e) => setAsChangeRequest(e.target.checked)}
              disabled={disabled}
            />
            Request changes
          </label>
          <Button
            variant="primary"
            disabled={disabled || submitting || !note.trim()}
            onClick={onSubmit}
          >
            {submitting ? "Adding…" : asChangeRequest ? "Request changes" : "Comment"}
          </Button>
        </div>
      </div>
    </div>
  );
}

/// A colored status chip for a proposal, so state reads at a glance instead of as
/// dim uppercase text (open = neutral, approved = success, rejected = danger,
/// applied = accent).
function StatusPill({ status }: { status: string }) {
  const tone: Tone =
    status === "approved" || status === "applied"
      ? "success"
      : status === "rejected"
        ? "danger"
        : status === "open"
          ? "info"
          : "neutral";
  const icon =
    tone === "success" ? <IconCheck size={11} /> : tone === "danger" ? <IconX size={11} /> : undefined;
  return (
    <Badge tone={tone} icon={icon}>
      {status}
    </Badge>
  );
}

function ReviewPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const proposals = useQuery({
    queryKey: ["proposals", sessionId],
    queryFn: () => listProposals(sessionId),
  });
  // Settled work is history, so it starts collapsed; dismissed work is history
  // the reader has explicitly filed away, so it stays out of the way until asked
  // for. Neither is deleted — both shelves can be reopened.
  const [showSettled, setShowSettled] = useState(false);
  const [showDismissed, setShowDismissed] = useState(false);

  // Alternatives of a proposal set render inside their set, not on their own.
  const sets = useProposalSets(sessionId);
  const groups = groupProposals(withoutSetAlternatives(proposals.data ?? [], sets.data ?? []));
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    qc.invalidateQueries({ queryKey: ["chat", sessionId] });
  };
  const dismiss = async (ids: string[], dismissed: boolean) => {
    try {
      await dismissProposals(sessionId, ids, dismissed);
      qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    } catch (e) {
      toast.error(`Couldn't ${dismissed ? "dismiss" : "restore"}: ${errMsg(e)}`);
    }
  };
  const card = (proposal: ProposalDto) => (
    <ProposalCard
      key={proposal.id}
      proposal={proposal}
      sessionId={sessionId}
      onChanged={refresh}
      onDismiss={(dismissed) => dismiss([proposal.id], dismissed)}
    />
  );

  const loading = proposals.isPending || sets.isPending;
  const failed = proposals.isError || sets.isError;
  return (
    <RailFrame title="Review" about="Pending decisions and proposal quorum for this chat." docs={PANE_DOCS.review}>
      <AgentAsks sessionId={sessionId} />
      <QueuedWorkSection />
      {failed && (
        <div className="mb-4">
          <ErrorState
            text="Couldn't load proposals for this chat."
            onRetry={() => {
              void proposals.refetch();
              void sets.refetch();
            }}
          />
        </div>
      )}
      {loading && !failed && <LoadingState rows={3} label="Loading proposals" />}
      {!loading && !failed && (
      <>
      {(sets.data ?? []).length > 0 && (
        <Section title="Proposal sets">
          <ProposalSets sessionId={sessionId} sets={sets.data ?? []} />
        </Section>
      )}
      <Section title="Pending proposals">
        {groups.active.map(card)}
        {groups.active.length === 0 && (
          <EmptyHint icon={<IconInbox size={18} />} text="Nothing is waiting for review." />
        )}
      </Section>

      {groups.settled.length > 0 && (
        <Section
          title="Settled"
          action={
            <button
              className="text-xs text-ink-soft transition-colors hover:text-ink"
              onClick={() => dismiss(groups.settled.map((p) => p.id), true)}
            >
              Clear settled
            </button>
          }
        >
          <FormDisclosure
            label={`${groups.settled.length} settled proposal${groups.settled.length === 1 ? "" : "s"}`}
            open={showSettled}
            onToggle={() => setShowSettled((v) => !v)}
          >
            <div className="space-y-2.5">{groups.settled.map(card)}</div>
          </FormDisclosure>
        </Section>
      )}

      {groups.dismissed.length > 0 && (
        <Section title="Dismissed">
          <FormDisclosure
            label={`${groups.dismissed.length} dismissed`}
            open={showDismissed}
            onToggle={() => setShowDismissed((v) => !v)}
          >
            <div className="space-y-2.5">{groups.dismissed.map(card)}</div>
          </FormDisclosure>
        </Section>
      )}
          </>
      )}
    </RailFrame>
  );
}

const MEMBER_ROLES = ["owner", "admin", "contributor", "viewer"];
/// A host a person can name in "Apply on ‹host›": one with a workspace device id.
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function PeoplePane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const members = useQuery({ queryKey: ["members", sessionId], queryFn: () => listMembers(sessionId) });
  // Presence only exists with a relay; skip the poll entirely when solo so we
  // don't burn IPC round-trips (helps the Windows shell stay responsive).
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const presence = useQuery({
    queryKey: ["presence", sessionId],
    queryFn: presenceList,
    enabled: Boolean(sync.data?.relayConfigured),
    refetchInterval: sync.data?.relayConfigured ? 5000 : false,
  });
  const onlineActors = new Set((presence.data ?? []).map((p) => p.actorId));
  // Registered devices/workers for this workspace. Shares the ["workspace-hosts"]
  // cache with the queue's "run on worker" menu. Poll on the same cadence as
  // member presence so online/offline + "last seen" stay live (only with a
  // relay — solo has no remote hosts to refresh).
  const hosts = useQuery({
    queryKey: ["workspace-hosts"],
    queryFn: listWorkspaceHosts,
    refetchInterval: sync.data?.relayConfigured ? 5000 : false,
  });

  // The People list shows *collaborators*, never yourself: you're in the
  // identity card, and a self row would offer Remove / Remove & revoke on
  // yourself — a footgun (the last owner can't be removed anyway, and "revoke
  // myself" would rotate a key you keep). Leaving a workspace is the workspace
  // rail's job. Matches the sidebar People count, which already excludes self.
  const collaborators = (members.data ?? []).filter((m) => !m.isSelf);
  // Your own membership row — shown so a solo/owned workspace doesn't read as
  // empty ("0 people / no extra members"). Absent means you aren't in this
  // workspace's roster (e.g. it was created under a different/previous identity).
  const selfMember = (members.data ?? []).find((m) => m.isSelf);

  // Show `#index` only on names that actually collide; matchable as `@Name#N`.
  const nameCounts = new Map<string, number>();
  for (const m of collaborators) {
    nameCounts.set(m.displayName, (nameCounts.get(m.displayName) ?? 0) + 1);
  }

  const [error, setError] = useState<string | null>(null);
  // An invite link is how a person joins: single-use, issued by an Admin on
  // the config log, carrying your safety code. Open by default.
  const [showInvite, setShowInvite] = useState(true);
  const [inviteLink, setInviteLink] = useState<string | null>(null);
  const workspacesQ = useQuery({ queryKey: ["workspaces"], queryFn: listWorkspaces });
  const activeRoom = (workspacesQ.data ?? []).find((w) => w.active && w.kind === "room");
  const refresh = () => qc.invalidateQueries({ queryKey: ["members", sessionId] });

  const setRoleMutation = useMutation({
    mutationFn: (v: { id: string; role: string }) => setMemberRole(sessionId, v.id, v.role),
    onSuccess: () => {
      setError(null);
      refresh();
    },
    onError: (e) => setError(String(e)),
  });
  // Rules v2: a member whose passkeys were all removed (or who never had
  // one) registers a first passkey only after an Admin's re-grant.
  const regrantMutation = useMutation({
    mutationFn: (id: string) => passkeyRegrant(id),
    onSuccess: () => {
      setError(null);
      toast.success("They can register a new passkey now.");
    },
    onError: (e) => toast.error(`Couldn't re-grant: ${errMsg(e)}`),
  });
  // Issue #118: a removal can also take the member's shared files down (a
  // signed deletion of each, then their blobs off the relay).
  const offerFileCleanup = async (id: string) => {
    const ok = await confirmDialog("Also delete the files they shared in this workspace? Everyone stops seeing them and their encrypted copies leave the relay.", {
      title: "Their shared files",
      confirmLabel: "Delete their files",
      danger: true,
    });
    if (!ok) return;
    try {
      const n = await deleteMemberAttachments(id);
      toast.success(n === 1 ? "Deleted 1 file." : `Deleted ${n} files.`);
    } catch (e) {
      toast.error(`Couldn't delete their files: ${errMsg(e)}`);
    }
  };
  const removeMutation = useMutation({
    mutationFn: (id: string) => removeMember(sessionId, id),
    onSuccess: (_r, id) => {
      setError(null);
      refresh();
      void offerFileCleanup(id);
    },
    onError: (e) => setError(String(e)),
  });
  // Owner/admin "kick a bad actor": remove + rotate the workspace key so they
  // lose access to new messages.
  const revokeMutation = useMutation({
    mutationFn: (id: string) => removeAndRevoke(sessionId, id),
    onSuccess: (r, id) => {
      setError(null);
      refresh();
      void offerFileCleanup(id);
      toast.success(
        r.rotated
          ? `Removed and revoked — key rotated to ${r.recipients} member${r.recipients === 1 ? "" : "s"}.`
          : "Removed. (No relay configured, so the key wasn't rotated.)",
      );
    },
    onError: (e) => toast.error(`Couldn't revoke: ${errMsg(e)}`),
  });
  const inviteMutation = useMutation({
    mutationFn: () => {
      if (!activeRoom) throw new Error("Invites are for team workspaces; switch to one first.");
      return workspaceInvite(activeRoom.id);
    },
    onSuccess: (code) => {
      setError(null);
      setInviteLink(code);
      void navigator.clipboard.writeText(code).catch(() => {});
      toast.success("Invite link copied. It works once.");
    },
    onError: (e) => {
      const m = inviteError(e);
      toast.error(m.action ? m.message : `Couldn't create an invite: ${m.message}`, m.action);
    },
  });

  // Server-side membership (enforced by a relay with membership on). Distinct from
  // the roster above: this is who the *relay* will accept writes from.
  const relayOn = Boolean(sync.data?.relayConfigured);
  const srvMembers = useQuery({
    queryKey: ["server-members"],
    queryFn: workspaceMembers,
    enabled: relayOn,
  });
  const audit = useQuery({ queryKey: ["membership-audit", sessionId], queryFn: listMembershipAudit });
  const [showAudit, setShowAudit] = useState(false);
  const refreshSrv = () => qc.invalidateQueries({ queryKey: ["server-members"] });
  const claimMutation = useMutation({
    mutationFn: workspaceClaimMembership,
    onSuccess: (claimed) => {
      refreshSrv();
      toast.success(
        claimed
          ? "Membership enabled — you're the owner. The relay now enforces who can write."
          : "This relay doesn't enforce membership (open/self-host), or it's already enabled.",
      );
    },
    onError: (e) => toast.error(`Couldn't claim: ${errMsg(e)}`),
  });
  const srvRemoveMutation = useMutation({
    mutationFn: (account: string) => workspaceRemoveMember(account),
    onSuccess: (r) => {
      refreshSrv();
      refresh();
      if (r.unmapped) {
        toast.error(
          "Removed on the relay and rotated the key, but this entry matches no workspace member here, so it may still read new messages. If it's a box, remove its member under Members (Remove & revoke).",
        );
      } else if (r.closedAdmission) {
        toast.success(`Removed. The box never joined; its admission is closed. Key rotated to ${r.recipients} device${r.recipients === 1 ? "" : "s"}.`);
      } else {
        toast.success(`Removed from the relay and the workspace. Key rotated to ${r.recipients} device${r.recipients === 1 ? "" : "s"}.`);
      }
    },
    onError: (e) => toast.error(`Couldn't remove: ${errMsg(e)}`),
  });

  // Relay invites — revocable/expiring self-enroll codes (Admin+ issues).
  const invites = useQuery({
    queryKey: ["workspace-invites"],
    queryFn: workspaceListInvites,
    enabled: relayOn,
  });
  const refreshInvites = () => qc.invalidateQueries({ queryKey: ["workspace-invites"] });
  const [invRole, setInvRole] = useState("contributor");
  const [invDays, setInvDays] = useState("7");
  const [invUses, setInvUses] = useState("1");
  const [lastCode, setLastCode] = useState<string | null>(null);
  const createInviteMutation = useMutation({
    mutationFn: () =>
      workspaceCreateInvite(invRole, (Number(invDays) || 0) * 86400, Number(invUses) || 0),
    onSuccess: (inv) => {
      refreshInvites();
      if (inv) {
        setLastCode(inv.code);
        void navigator.clipboard.writeText(inv.code).catch(() => {});
        toast.success("Invite created + code copied. Share it with the code — it's shown once.");
      } else {
        toast.error("This relay doesn't support invites (open/self-host), or you're not an admin.");
      }
    },
    onError: (e) => toast.error(`Couldn't create invite: ${errMsg(e)}`),
  });
  const revokeInviteMutation = useMutation({
    mutationFn: (id: string) => workspaceRevokeInvite(id),
    onSuccess: () => {
      refreshInvites();
      toast.success("Invite revoked.");
    },
    onError: (e) => toast.error(`Couldn't revoke: ${errMsg(e)}`),
  });
  const [joinCode, setJoinCode] = useState("");
  const joinMutation = useMutation({
    mutationFn: () => workspaceJoinViaInvite(joinCode.trim()),
    onSuccess: (role) => {
      if (role) {
        setJoinCode("");
        refreshSrv();
        toast.success(`Joined as ${role}.`);
      } else {
        toast.error("That invite code was refused (invalid, expired, revoked, or used up).");
      }
    },
    onError: (e) => toast.error(`Couldn't join: ${errMsg(e)}`),
  });

  // Remote-agent bootstrap generator (relay admins with an issuer key).
  const issuerKey = useQuery({ queryKey: ["has-issuer-key"], queryFn: hasIssuerKey, enabled: relayOn });
  const [agentRole, setAgentRole] = useState("contributor");
  const [agentLabel, setAgentLabel] = useState("");
  const [agentBlock, setAgentBlock] = useState<string | null>(null);
  const bootstrapMutation = useMutation({
    mutationFn: () =>
      generateAgentBootstrap(agentRole, agentLabel.trim(), 365),
    onSuccess: (block) => {
      setAgentBlock(block);
      refreshSrv();
      void navigator.clipboard.writeText(block).catch(() => {});
      toast.success("Bootstrap command generated + copied. Paste it on the agent box.");
    },
    onError: (e) => toast.error(`Couldn't generate: ${errMsg(e)}`),
  });

  return (
    <RailFrame title="People" about="Workspace members and governance roles for this chat." docs={PANE_DOCS.people}>
      <Section title="Members">
        {error && (
          <div className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
            {error}
          </div>
        )}
        {selfMember ? (
          <Card className="flex items-center justify-between px-3 py-2">
            <div className="min-w-0">
              <div className="font-medium">
                {selfMember.displayName} <span className="text-xs text-ink-soft">· you</span>
              </div>
              <div className="text-xs text-ink-soft">{selfMember.role}</div>
            </div>
          </Card>
        ) : (
          <div className="rounded-xl border px-3 py-2 text-xs" style={{ borderColor: "var(--ligus-line)", color: "var(--ligus-warm-ink)" }}>
            You aren't in this workspace's roster — it was created under a different identity. If it's
            your own, re-create it; identity is stable going forward.
          </div>
        )}
        {collaborators.map((member) => {
          const online = onlineActors.has(member.actorId);
          const dup = (nameCounts.get(member.displayName) ?? 0) > 1 && member.index > 0;
          return (
            <Card key={member.id} className="px-3 py-3">
              <div className="flex items-center gap-2 font-medium">
                <span className="relative shrink-0">
                  <Avatar name={member.displayName} url={member.avatarUrl} kind="human" size={26} />
                  <span className="absolute -bottom-0.5 -right-0.5 inline-flex rounded-full" style={{ background: "var(--ligus-panel)" }}>
                    <StatusDot
                      on={online}
                      size={9}
                      label={sync.data?.relayConfigured ? (online ? "Online" : "Offline") : "Presence needs a relay"}
                    />
                  </span>
                </span>
                {member.displayName}
                {dup && (
                  <span
                    className="text-xs text-ink-soft"
                    data-tip={`Mention precisely with @${member.displayName}#${member.index}`}
                  >
                    #{member.index}
                  </span>
                )}
                {member.title && <span className="text-xs text-ink-soft">· {member.title}</span>}
                {member.isFounder && (
                  <Badge title="The workspace's founder: nobody can remove or demote them">founder</Badge>
                )}
                {member.kind === "service" && (
                  <Badge title="A box (ligus serve or a worker): its own member, at most Contributor">service</Badge>
                )}
              </div>
              <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
                {(() => {
                  // The backend computes each refusal with the fold's own rules
                  // (an older backend sends none: everything stays offered).
                  const refusalOf = (r: string) => member.roleOptions?.find((o) => o.role === r)?.refusal ?? null;
                  const others = MEMBER_ROLES.filter((r) => r !== member.role);
                  const allRefused = (member.roleOptions?.length ?? 0) > 0 && others.every((r) => refusalOf(r));
                  return (
                    <SubtleSelectField
                      label="Role"
                      value={member.role}
                      disabled={allRefused}
                      title={allRefused ? `You can't change ${member.displayName}'s role: ${refusalOf(others[0])}` : undefined}
                      onChange={(r) => setRoleMutation.mutate({ id: member.id, role: r })}
                    >
                      {MEMBER_ROLES.map((r) => {
                        const why = r === member.role ? null : refusalOf(r);
                        return (
                          <option key={r} value={r} disabled={Boolean(why)} title={why ?? undefined}>
                            {r}
                          </option>
                        );
                      })}
                    </SubtleSelectField>
                  );
                })()}
                <button
                  className="shrink-0 rounded text-xs text-ink-soft hover:text-ink hover:underline disabled:cursor-not-allowed disabled:opacity-50 disabled:no-underline"
                  disabled={Boolean(member.removeRefusal)}
                  onClick={() =>
                    confirmThen("Remove this member from the roster?", () =>
                      removeMutation.mutate(member.id),
                    )
                  }
                  data-tip={member.removeRefusal ?? "Remove from the member list only"}
                >
                  Remove
                </button>
                <button
                  className="shrink-0 rounded text-xs text-ink-soft hover:text-ink hover:underline disabled:cursor-not-allowed disabled:opacity-50 disabled:no-underline"
                  disabled={Boolean(member.regrantRefusal)}
                  onClick={() => regrantMutation.mutate(member.id)}
                  data-tip={
                    member.regrantRefusal ??
                    "Under rules v2, let them register a first passkey (after theirs were removed). Confirmed with your passkey."
                  }
                >
                  Allow new passkey
                </button>
                <button
                  className="shrink-0 text-xs font-medium hover:opacity-80 disabled:cursor-not-allowed disabled:opacity-50"
                  style={{ color: "var(--ligus-danger-ink)" }}
                  disabled={Boolean(member.removeRefusal)}
                  onClick={() =>
                    confirmThen(
                      `Remove ${member.displayName} and revoke their access? The workspace key will be rotated so they can't read new messages.`,
                      () => revokeMutation.mutate(member.id),
                    )
                  }
                  data-tip={member.removeRefusal ?? "Remove and rotate the key so they lose access (for a bad actor)"}
                >
                  Remove &amp; revoke
                </button>
              </div>
            </Card>
          );
        })}
        {collaborators.length === 0 && (
          <EmptyHint text="No extra members have been added yet." />
        )}

        <FormDisclosure label="Invite someone" open={showInvite} onToggle={() => setShowInvite((v) => !v)}>
          <Card className="space-y-2 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
              Make a single-use invite link and send it over a channel you trust. It carries your safety code so they
              can check it came from you. To invite someone you already share another workspace with, use People.
            </p>
            <Button variant="primary" disabled={!activeRoom || inviteMutation.isPending} onClick={() => inviteMutation.mutate()}>
              {inviteMutation.isPending ? "…" : "Create invite link"}
            </Button>
            {inviteLink && (
              <textarea
                readOnly
                value={inviteLink}
                rows={3}
                className="w-full rounded-xl border px-3 py-2 font-mono text-[11px]"
                style={fieldStyle}
                aria-label="Invite link"
              />
            )}
          </Card>
        </FormDisclosure>

        <FormDisclosure label="Membership history" open={showAudit} onToggle={() => setShowAudit((v) => !v)}>
          <Card className="space-y-1.5 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
              A tamper-evident audit from the signed config log — who joined or was removed, by whom,
              and when.
            </p>
            {(audit.data ?? []).length === 0 ? (
              <EmptyHint text="No membership changes recorded yet." />
            ) : (
              (audit.data ?? []).map((e, i) => (
                <div key={i} className="flex items-baseline justify-between gap-2 text-xs">
                  <span className="min-w-0 truncate">
                    <span style={{ color: e.action === "removed" ? "var(--ligus-danger-ink)" : "var(--ligus-accent-warm)" }}>
                      {e.action === "removed" ? "removed" : `added${e.role ? ` (${e.role})` : ""}`}
                    </span>{" "}
                    <b>{e.member}</b> <span className="text-ink-soft">by {e.by}</span>
                  </span>
                  <span className="shrink-0 text-ink-soft" data-tip={e.at}>
                    {new Date(e.at).toLocaleDateString()}
                  </span>
                </div>
              ))
            )}
          </Card>
        </FormDisclosure>
      </Section>

      <Section title="Team members (server-enforced)">
        {!relayOn ? (
          <EmptyHint text="Connect a relay (Settings → Team sync) to use server-enforced membership." />
        ) : (
          <>
            <p className="text-xs text-ink-soft">
              Who the relay accepts writes from. Requires a relay with membership turned on
              (LANGSTROTH_RELAY_MEMBERSHIP=1); otherwise this stays empty and everyone can write.
            </p>
            {(srvMembers.data ?? []).map((m) => (
              <Card key={m.account} className="flex items-center justify-between px-3 py-2">
                <div className="min-w-0">
                  <div className="truncate font-medium">@{m.login}</div>
                  <div className="text-xs text-ink-soft">
                    {m.role} · {m.account}
                  </div>
                </div>
                <button
                  className="shrink-0 text-xs font-medium hover:opacity-80"
                  style={{ color: "var(--ligus-danger-ink)" }}
                  onClick={() =>
                    confirmThen(`Remove @${m.login} from this workspace? They're removed on the relay and from the workspace, then the key is rotated so they can't read new messages.`, () =>
                      srvRemoveMutation.mutate(m.account),
                    )
                  }
                  data-tip="Remove them on the relay and from the workspace, then rotate the key"
                >
                  Remove
                </button>
              </Card>
            ))}
            {(srvMembers.data ?? []).length === 0 && (
              <Card className="space-y-2 p-3">
                <div className="text-sm font-semibold">Enable member controls</div>
                <p className="text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
                  Claim this workspace on the relay so it enforces who can write — you become the
                  owner. No-op on an open relay.
                </p>
                <Button variant="primary" disabled={claimMutation.isPending} onClick={() => claimMutation.mutate()}>
                  {claimMutation.isPending ? "…" : "Claim ownership"}
                </Button>
              </Card>
            )}
          </>
        )}
      </Section>

      <Section title="Invites">
        {!relayOn ? (
          <EmptyHint text="Connect a relay (Settings → Team sync) to issue invites." />
        ) : (
          <>
            <p className="text-xs text-ink-soft">
              Share a code so someone can join at a set role. Codes are revocable, can expire, and
              can cap how many times they're used. The code shows once, on creation.
            </p>
            {/* Issue */}
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <select
                value={invRole}
                onChange={(e) => setInvRole(e.target.value)}
                className="rounded-lg border px-2 py-1.5 text-sm"
                style={fieldStyle}
              >
                <option value="viewer">viewer</option>
                <option value="contributor">contributor</option>
                <option value="admin">admin</option>
              </select>
              <label className="flex items-center gap-1 text-xs text-ink-soft">
                days
                <input value={invDays} onChange={(e) => setInvDays(e.target.value)} inputMode="numeric" className="w-12 rounded-lg border px-2 py-1 text-sm" style={fieldStyle} />
              </label>
              <label className="flex items-center gap-1 text-xs text-ink-soft">
                uses
                <input value={invUses} onChange={(e) => setInvUses(e.target.value)} inputMode="numeric" className="w-12 rounded-lg border px-2 py-1 text-sm" style={fieldStyle} />
              </label>
              <Button variant="primary" disabled={createInviteMutation.isPending} onClick={() => createInviteMutation.mutate()}>
                Create invite
              </Button>
            </div>
            <p className="mt-1 text-[11px] text-ink-soft">0 days = never expires · 0 uses = unlimited.</p>
            {lastCode && (
              <div
                className="mt-2 flex items-center justify-between gap-2 rounded-lg border px-2.5 py-1.5"
                style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
              >
                <code className="truncate text-xs">{lastCode}</code>
                <button className="shrink-0 text-xs underline hover:opacity-80" onClick={() => void navigator.clipboard.writeText(lastCode).catch(() => {})}>
                  Copy
                </button>
              </div>
            )}
            {/* Existing invites */}
            {(invites.data ?? []).filter((i) => !i.revoked).map((i: InviteEntry) => (
              <div key={i.id} className="mt-1.5 flex items-center justify-between gap-2 text-sm">
                <span className="min-w-0">
                  <span className="font-medium">{i.role}</span>{" "}
                  <span className="text-xs text-ink-soft">
                    · {i.maxUses === 0 ? `${i.uses} uses` : `${i.uses}/${i.maxUses} used`}
                    {i.expiresAt > 0 ? ` · expires ${new Date(i.expiresAt * 1000).toLocaleDateString()}` : ""}
                  </span>
                </span>
                <button className="shrink-0 text-xs underline-offset-2 hover:underline" style={{ color: "var(--ligus-danger-ink)" }} onClick={() => revokeInviteMutation.mutate(i.id)}>
                  Revoke
                </button>
              </div>
            ))}
            {/* Redeem (for a joiner) */}
            <div className="mt-3 flex items-center gap-2">
              <input
                value={joinCode}
                onChange={(e) => setJoinCode(e.target.value)}
                placeholder="Have a code? Paste it to join"
                className="flex-1 rounded-lg border px-2.5 py-1.5 font-mono text-xs"
                style={fieldStyle}
                onKeyDown={(e) => e.key === "Enter" && joinCode.trim() && joinMutation.mutate()}
              />
              <Button variant="ghost" disabled={!joinCode.trim() || joinMutation.isPending} onClick={() => joinMutation.mutate()}>
                Join
              </Button>
            </div>
          </>
        )}
      </Section>

      <Section title="Remote agents">
        {!relayOn ? (
          <EmptyHint text="Connect a relay (Settings → Team sync) to add remote agents." />
        ) : !issuerKey.data ? (
          <p className="text-xs text-ink-soft">
            To generate a one-paste command for a headless agent, add your relay issuer key in
            Settings → Team sync (advanced). The key mints the agent's identity token locally; only
            the token travels to the box.
          </p>
        ) : (
          <>
            <p className="text-xs text-ink-soft">
              Enroll a remote agent and generate the exact command a developer runs on its box —
              installs the CLI, connects to this workspace, and starts the worker.
            </p>
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <select
                value={agentRole}
                onChange={(e) => setAgentRole(e.target.value)}
                className="rounded-lg border px-2 py-1.5 text-sm"
                style={fieldStyle}
              >
                <option value="viewer">viewer</option>
                <option value="contributor">contributor</option>
                <option value="admin">admin</option>
              </select>
              <input
                value={agentLabel}
                onChange={(e) => setAgentLabel(e.target.value)}
                placeholder="box label (optional)"
                className="w-28 rounded-lg border px-2.5 py-1.5 text-sm"
                style={fieldStyle}
              />
              <Button
                variant="primary"
                disabled={bootstrapMutation.isPending}
                onClick={() => bootstrapMutation.mutate()}
              >
                Generate
              </Button>
            </div>
            {agentBlock && (
              <div className="mt-2">
                <textarea
                  readOnly
                  value={agentBlock}
                  spellCheck={false}
                  className="h-28 w-full rounded-lg border p-2 font-mono text-[11px]"
                  style={fieldStyle}
                />
                <button
                  className="mt-1 text-xs underline text-ink-soft hover:text-ink"
                  onClick={() => void navigator.clipboard.writeText(agentBlock).catch(() => {})}
                >
                  Copy again
                </button>
              </div>
            )}
          </>
        )}
      </Section>

      <Section title="Workers & hosts">
        {(hosts.data ?? []).length === 0 ? (
          <EmptyHint text="No hosts registered." />
        ) : (
          (hosts.data ?? []).map((host) => <HostRow key={host.id} host={host} />)
        )}
      </Section>
    </RailFrame>
  );
}

/// A single registered device/worker: label, a kind chip, an online/offline
/// status dot, and a relative "last seen".
function HostRow({ host }: { host: WorkspaceHostDto }) {
  const gateways = hostGatewaysLabel(host);
  return (
    <Card className="px-3 py-2">
    <div className="flex items-center justify-between gap-2">
      <div className="flex min-w-0 items-center gap-2">
        <StatusDot on={host.online} size={9} label={host.online ? "Online" : "Offline"} />
        <span className="truncate font-medium">{host.label || "Unnamed host"}</span>
        <span
          className="shrink-0 rounded-full px-1.5 py-0.5 text-[11px] font-semibold uppercase tracking-wider"
          style={{
            background: "color-mix(in srgb, var(--ligus-accent-cool) 15%, transparent)",
            color: "var(--ligus-accent-ink)",
          }}
        >
          {host.kind}
        </span>
      </div>
      <span className="shrink-0 text-[11px]" style={{ color: "var(--ligus-ink-soft)" }}>
        {host.online ? "online" : relativeAgo(host.lastSeen)}
      </span>
    </div>
    {gateways && (
      <div className="mt-1 truncate text-[11px]" style={{ color: "var(--ligus-ink-soft)" }} data-tip="Model gateways this host's operator lists">
        Gateways: {gateways}
      </div>
    )}
    </Card>
  );
}

/// Coarse "3m ago" / "2h ago" from an RFC 3339 timestamp. Empty/unparseable
/// input renders nothing meaningful ("—").
function relativeAgo(iso: string): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (secs < 60) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  return `${days}d ago`;
}

/** Human label for a skill/vault's agent targeting. Empty ⇒ every participant. */
function targetLabel(agentIds: string[], agents: WorkspaceAgentDto[]): string {
  if (agentIds.length === 0) return "All agents";
  const names = agentIds.map((id) => agents.find((a) => a.id === id)?.name ?? "unknown");
  return names.join(", ");
}

/**
 * Compact multi-select of workspace agents for scoping a skill/vault. No
 * selection ⇒ global (primary + every agent). Colors route through --ligus-*.
 */
function AgentTargetPicker({
  agents,
  selected,
  onChange,
}: {
  agents: WorkspaceAgentDto[];
  selected: string[];
  onChange: (ids: string[]) => void;
}) {
  const toggle = (id: string) =>
    onChange(selected.includes(id) ? selected.filter((x) => x !== id) : [...selected, id]);
  return (
    <div>
      <div className="mb-1 text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
        Applies to {selected.length === 0 ? "all agents (global)" : `${selected.length} agent(s)`}
      </div>
      {agents.length === 0 ? (
        <div className="text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
          No agents in this workspace — this stays global.
        </div>
      ) : (
        <div
          className="max-h-32 space-y-1 overflow-auto rounded-xl p-2"
          style={{ background: "var(--ligus-mist)" }}
        >
          {agents.map((agent) => (
            <label key={agent.id} className="flex cursor-pointer items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={selected.includes(agent.id)}
                onChange={() => toggle(agent.id)}
                style={{ accentColor: "var(--ligus-accent-cool)" }}
              />
              <span className="truncate">{agent.name}</span>
            </label>
          ))}
        </div>
      )}
    </div>
  );
}

/** Small pill showing a skill/vault's target scope on its row. */
function TargetBadge({ agentIds, agents }: { agentIds: string[]; agents: WorkspaceAgentDto[] }) {
  return (
    <span
      className="inline-block rounded-full px-2 py-0.5 text-[11px] font-medium"
      style={{ background: "var(--ligus-mist)", color: "var(--ligus-ink-soft)" }}
    >
      {targetLabel(agentIds, agents)}
    </span>
  );
}

function VaultsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const vaults = useQuery({ queryKey: ["vaults", sessionId], queryFn: () => listVaults(sessionId) });
  const agents = useQuery({ queryKey: ["agents", sessionId], queryFn: () => listAgents(sessionId) });
  const [kind, setKind] = useState("github");
  const [reference, setReference] = useState("");
  const [targetIds, setTargetIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<{ url: string; text: string } | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const addVaultMutation = useMutation({
    mutationFn: () => addVault(sessionId, kind, reference.trim(), targetIds),
    onSuccess: () => {
      setReference("");
      setTargetIds([]);
      setError(null);
      setShowAdd(false);
      qc.invalidateQueries({ queryKey: ["vaults", sessionId] });
    },
    onError: (mutationError) => setError(String(mutationError)),
  });
  const removeVaultMutation = useMutation({
    mutationFn: (url: string) => removeVault(sessionId, url),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["vaults", sessionId] }),
  });
  const previewVaultMutation = useMutation({
    mutationFn: (url: string) => previewVault(url).then((text) => ({ url, text })),
    onSuccess: setPreview,
    onError: (mutationError) => setError(String(mutationError)),
  });
  const placeholder = kind === "https" ? "https://…/file.md" : "owner/repo/path.md@main";

  return (
    <RailFrame title="Vaults" about="Reference material mounted into this workspace." docs={PANE_DOCS.vaults}>
      <Section title="Sources">
        {(vaults.data ?? []).map((vault) => (
          <Card key={vault.url} className="px-3 py-3">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="font-medium">{vault.label}</div>
                <div className="mt-1 truncate text-xs text-ink-soft">
                  {vault.kind} · {vault.url}
                </div>
                <div className="mt-1.5">
                  <TargetBadge agentIds={vault.agentIds} agents={agents.data ?? []} />
                </div>
              </div>
              <div className="flex gap-2 text-xs">
                <button className="text-ink-soft hover:text-ink" onClick={() => previewVaultMutation.mutate(vault.url)}>
                  Preview
                </button>
                <button
                  className="hover:opacity-80"
                  style={{ color: "var(--ligus-danger-ink)" }}
                  onClick={() => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url))}
                >
                  Remove
                </button>
              </div>
            </div>
            {preview?.url === vault.url && (
              <pre
                className="mt-3 max-h-56 overflow-auto rounded-2xl p-3 text-xs leading-5"
                style={{ background: "var(--ligus-mist)" }}
              >
                {preview.text}
              </pre>
            )}
          </Card>
        ))}
        {(vaults.data ?? []).length === 0 && (
          <EmptyHint text="No vault sources are mounted in this chat." />
        )}
        <FormDisclosure label="Add vault" open={showAdd} onToggle={() => setShowAdd((v) => !v)}>
          <Card className="space-y-2 p-3">
            <SubtleSelectField label="Vault source" value={kind} onChange={setKind}>
              <option value="github">GitHub</option>
              <option value="gitlab">GitLab</option>
              <option value="https">HTTPS</option>
            </SubtleSelectField>
            <input
              value={reference}
              onChange={(event) => setReference(event.target.value)}
              placeholder={placeholder}
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <AgentTargetPicker
              agents={agents.data ?? []}
              selected={targetIds}
              onChange={setTargetIds}
            />
            {error && (
              <div className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
                {error}
              </div>
            )}
            <Button variant="primary" disabled={!reference.trim() || addVaultMutation.isPending} onClick={() => addVaultMutation.mutate()}>
              Add
            </Button>
          </Card>
        </FormDisclosure>
      </Section>
    </RailFrame>
  );
}

function SkillsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const skills = useQuery({ queryKey: ["skills", sessionId], queryFn: () => listSkills(sessionId) });
  const agents = useQuery({ queryKey: ["agents", sessionId], queryFn: () => listAgents(sessionId) });
  const [name, setName] = useState("");
  const [source, setSource] = useState("");
  const [targetIds, setTargetIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const installSkillMutation = useMutation({
    mutationFn: () => installSkill(sessionId, name.trim(), source.trim(), targetIds),
    onSuccess: () => {
      setName("");
      setSource("");
      setTargetIds([]);
      setError(null);
      setShowAdd(false);
      qc.invalidateQueries({ queryKey: ["skills", sessionId] });
    },
    onError: (mutationError) => setError(String(mutationError)),
  });
  const removeSkillMutation = useMutation({
    mutationFn: (skillId: string) => removeSkill(sessionId, skillId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["skills", sessionId] }),
  });

  return (
    <RailFrame title="Skills" about="Instruction bundles injected into the active participants." docs={PANE_DOCS.skills}>
      <Section title="Loaded Skills">
        {(skills.data ?? []).map((skill) => (
          <Card key={skill.id} className="px-3 py-3">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="font-medium">{skill.name}</div>
                {skill.sourceUrl && <div className="mt-1 truncate text-xs text-ink-soft">{skill.sourceUrl}</div>}
                <div className="mt-2 line-clamp-3 text-xs leading-5 text-ink-soft">{skill.instructions}</div>
                <div className="mt-1.5">
                  <TargetBadge agentIds={skill.agentIds} agents={agents.data ?? []} />
                </div>
              </div>
              <button
                onClick={() => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id))}
                className="text-xs hover:opacity-80"
                style={{ color: "var(--ligus-danger-ink)" }}
              >
                Remove
              </button>
            </div>
          </Card>
        ))}
        {(skills.data ?? []).length === 0 && <EmptyHint text="No skills loaded for this chat." />}
        <FormDisclosure label="Install skill" open={showAdd} onToggle={() => setShowAdd((v) => !v)}>
          <Card className="space-y-2 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
              Supports direct URLs, GitHub blob URLs, and owner/repo/path references.
            </p>
            <input
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Display name (optional)"
              className="w-full rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
            <input
              value={source}
              onChange={(event) => setSource(event.target.value)}
              placeholder="skills.sh/... or owner/repo/SKILL.md"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <AgentTargetPicker
              agents={agents.data ?? []}
              selected={targetIds}
              onChange={setTargetIds}
            />
            {error && (
              <div className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
                {error}
              </div>
            )}
            <Button variant="primary" disabled={!source.trim() || installSkillMutation.isPending} onClick={() => installSkillMutation.mutate()}>
              Install
            </Button>
          </Card>
        </FormDisclosure>
      </Section>
    </RailFrame>
  );
}

function ActivityPane() {
  return (
    <RailFrame title="Activity" about="Recent runtime activity (agents, sync, errors) + the live chat stream." docs={PANE_DOCS.activity}>
      <div className="h-[calc(100vh-12rem)] min-h-[18rem] rounded-2xl border p-1.5" style={panelStyle}>
        <div className="h-full overflow-hidden rounded-xl">
          <LogsView />
        </div>
      </div>
    </RailFrame>
  );
}

// ─── Context pane ────────────────────────────────────────────────────────────

function fmtTokens(n: number): string {
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function runtimePickerLabel(runtime: RuntimeSummaryDto) {
  const provider = runtime.provider.trim();
  const model = runtime.model.trim();
  if (provider && model) return `${provider} / ${model}`;
  if (provider) return provider;
  if (model) return model;
  return runtime.label.trim() || runtime.name.trim() || "runtime";
}

function ContextPane({
  sessionId,
  activeRuntimeId,
}: {
  sessionId: string;
  activeRuntimeId: string;
}) {
  const qc = useQueryClient();
  const telemetry = useQuery({
    queryKey: ["context-telemetry", sessionId, activeRuntimeId],
    queryFn: () => getContextTelemetry(sessionId),
  });
  const skills = useQuery({ queryKey: ["skills", sessionId], queryFn: () => listSkills(sessionId) });
  const vaults = useQuery({ queryKey: ["vaults", sessionId], queryFn: () => listVaults(sessionId) });

  useEffect(() => {
    const unlisten = onChatStream((event) => {
      if (event.sessionId !== sessionId) return;
      qc.invalidateQueries({ queryKey: ["context-telemetry", sessionId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, sessionId]);

  const removeSkillMutation = useMutation({
    mutationFn: (id: string) => removeSkill(sessionId, id),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["skills", sessionId] });
      await qc.invalidateQueries({ queryKey: ["context-telemetry", sessionId] });
    },
  });
  const removeVaultMutation = useMutation({
    mutationFn: (url: string) => removeVault(sessionId, url),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["vaults", sessionId] });
      await qc.invalidateQueries({ queryKey: ["context-telemetry", sessionId] });
    },
  });

  const data = telemetry.data;
  const totalPlannedTokens = (data?.systemPromptTokens ?? 0) + (data?.keptTokens ?? 0);
  const budgetPct = data
    ? Math.min(100, Math.round((totalPlannedTokens / Math.max(1, data.contextWindowTokens)) * 100))
    : 0;
  const budgetColor =
    budgetPct >= 90
      ? "var(--ligus-danger)"
      : budgetPct >= 70
        ? "var(--ligus-accent-warm)"
        : "var(--ligus-accent-cool)";
  const hasCompaction = (data?.overflowMessageCount ?? 0) > 0;
  const summaryLabel =
    data?.summaryStrategy === "cached"
      ? "Using cached summary"
      : data?.summaryStrategy === "incremental"
        ? "Refreshing summary incrementally"
        : data?.summaryStrategy === "fresh"
          ? "Creating fresh summary"
          : "No compaction active";

  if (telemetry.isError) {
    return (
      <RailFrame title="Context" about="Real backend context budget and compaction state." docs={PANE_DOCS.context}>
        <EmptyHint text="Context telemetry is temporarily unavailable. Keep chatting and retry this tab." />
      </RailFrame>
    );
  }

  return (
    <RailFrame title="Context" about="Real backend context budget and compaction state." docs={PANE_DOCS.context}>
      <Section title="Budget">
        <div className="rounded-2xl border p-3" style={panelStyle}>
          <div className="mb-2 flex items-center justify-between text-xs">
            <span className="text-ink-soft">
              {data ? `${fmtTokens(totalPlannedTokens)} in prompt` : "Loading..."}
            </span>
            <span className="text-ink-soft">
              {data ? `${fmtTokens(data.contextWindowTokens)} window` : ""}
            </span>
          </div>
          <div className="h-2 overflow-hidden rounded-full" style={{ background: "var(--ligus-mist)" }}>
            <div
              className="h-full rounded-full transition-all duration-300"
              style={{ width: `${budgetPct}%`, background: budgetColor }}
            />
          </div>
          <div
            className="mt-1.5 text-right text-[11px] font-medium"
            style={{ color: budgetPct >= 90 ? "var(--ligus-danger-ink)" : budgetPct >= 70 ? "var(--ligus-warm-ink)" : "var(--ligus-ink-soft)" }}
          >
            {budgetPct}%
          </div>
          {data && (
            <div className="mt-2.5 grid grid-cols-2 gap-1.5 text-[11px] text-ink-soft">
              <span>System {fmtTokens(data.systemPromptTokens)}</span>
              <span className="text-right">History budget {fmtTokens(data.historyBudgetTokens)}</span>
              <span>Reserved output {fmtTokens(data.reservedOutputTokens)}</span>
              <span className="text-right">Summary reserve {fmtTokens(data.summaryReserveTokens)}</span>
            </div>
          )}
          {hasCompaction && data && (
            <div
              className="mt-2.5 rounded-xl px-3 py-2 text-xs leading-5"
              style={{
                background: "color-mix(in srgb, var(--ligus-accent-warm) 10%, transparent)",
                color: "var(--ligus-warm-ink)",
              }}
            >
              {data.overflowMessageCount} earlier messages are currently condensed to fit the context window.
            </div>
          )}
          {data && <div className="mt-2 text-[11px] text-ink-soft">{summaryLabel}</div>}
          {budgetPct >= 70 && (
            <div
              className="mt-2.5 rounded-xl px-3 py-2 text-xs leading-5"
              style={{
                background: "color-mix(in srgb, var(--ligus-danger) 10%, transparent)",
                color: "var(--ligus-danger-ink)",
              }}
            >
              Context is getting full — consider removing unused skills or vaults below.
            </div>
          )}
        </div>
        <div className="mt-2 grid grid-cols-3 gap-1.5 text-center">
          <ContextStat label="Messages" value={String(data?.messageCount ?? 0)} />
          <ContextStat label="Skills" value={String(data?.skillCount ?? 0)} />
          <ContextStat label="Vaults" value={String(data?.vaultCount ?? 0)} />
        </div>
      </Section>

      <Section title="Injected Skills">
        {(skills.data ?? []).length === 0 ? (
          <EmptyHint text="No skills loaded. Skills inject instructions into every turn." />
        ) : (
          (skills.data ?? []).map((skill) => {
            const est = Math.ceil(skill.instructions.length / 4);
            return (
              <div
                key={skill.id}
                className="flex items-start gap-2 rounded-xl border px-2.5 py-2"
                style={panelStyle}
              >
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium">{skill.name}</div>
                  <div className="mt-0.5 text-xs text-ink-soft">~{fmtTokens(est)} tokens in prompt</div>
                </div>
                <IconButton
                  label="Remove skill"
                  size={24}
                  onClick={() => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id))}
                >
                  <IconX size={14} />
                </IconButton>
              </div>
            );
          })
        )}
      </Section>

      <Section title="Mounted Vaults">
        {(vaults.data ?? []).length === 0 ? (
          <EmptyHint text="No vaults mounted. Vaults inject reference material into context." />
        ) : (
          (vaults.data ?? []).map((vault) => (
            <div
              key={vault.url}
              className="flex items-start gap-2 rounded-xl border px-2.5 py-2"
              style={panelStyle}
            >
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">{vault.label}</div>
                <div className="mt-0.5 text-xs text-ink-soft">{vault.kind} reference source</div>
              </div>
              <IconButton
                label="Unmount vault"
                size={24}
                onClick={() => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url))}
              >
                <IconX size={14} />
              </IconButton>
            </div>
          ))
        )}
      </Section>
    </RailFrame>
  );
}

// ─── Back-compat shims (retained deliberately) ───────────────────────────────
// `Stack`, `panelStyle` and `fieldStyle` are still imported by WorkflowsPane.tsx,
// and `SubtleSelectField` is kept for the same reason it always was. ui.tsx has no `Stack` and its `SubtleSelectField` shape differs, and its
// same-named `panelStyle`/`fieldStyle` tokens carry *swapped* panel/mist
// semantics — re-exporting them would invert contrast in those files and in this
// pane. So these keep RightRail's original values. RightRail's own new code
// composes the ui.tsx primitives (Button, Card, Section, SelectField, Switch,
// FormDisclosure, EmptyHint) instead. `EmptyHint` is re-exported from ui.tsx.

export { EmptyHint };

export function SubtleSelectField({
  label,
  value,
  onChange,
  children,
  disabled,
  title,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  children: ReactNode;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <label
      className="inline-flex w-full min-w-0 items-center gap-2.5 rounded-xl border px-2.5 py-1.5"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)", opacity: disabled ? 0.6 : 1 }}
      data-tip={title}
    >
      <span className="shrink-0 text-[11px] font-medium uppercase tracking-[0.12em] text-ink-soft">
        {label}
      </span>
      <select
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        className="min-w-0 flex-1 appearance-none bg-transparent text-sm font-medium outline-none"
        style={{ color: "var(--ligus-ink)", fontFamily: "inherit", backgroundImage: "none", paddingRight: 4 }}
      >
        {children}
      </select>
      <span className="shrink-0 text-ink-faint">
        <IconChevronDown size={13} />
      </span>
    </label>
  );
}

function ContextStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-xl border py-2 text-center" style={panelStyle}>
      <div className="text-base font-semibold">{value}</div>
      <div className="text-[11px] uppercase tracking-[0.12em] text-ink-soft">{label}</div>
    </div>
  );
}

export function Stack({ children }: { children: ReactNode }) {
  return <div className="space-y-2">{children}</div>;
}

export const panelStyle = {
  borderColor: "var(--ligus-line)",
  background: "var(--ligus-panel)",
} as const;

export const fieldStyle = {
  borderColor: "var(--ligus-line)",
  background: "var(--ligus-panel)",
  color: "var(--ligus-ink)",
} as const;
