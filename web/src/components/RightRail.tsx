import type { ReactNode } from "react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { SetupAction } from "@/components/setup/SetupAction";
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
  relayMembershipStatus,
  workspaceRemoveMember,
  workspaceClaimMembership,
  workspaceListInvites,
  workspaceCreateInvite,
  workspaceRevokeInvite,
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
  myProfile,
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
  archiveProposals,
  hideProposals,
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
import { groupProposals, implementRefusal, isSettled, isVotable, myVote } from "@/lib/proposals";
import { parseUnifiedDiff } from "@/lib/diff";
import { relTime } from "@/lib/time";
import { LogsView } from "@/components/LogsView";
import { Avatar } from "@/components/Avatar";
import { AgentAccessEditor, accessLabel, accessSummary } from "@/components/AgentAccessPanel";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { PRIMARY_NAME, PRIMARY_HANDLE } from "@/lib/agent";
import { toast, errMsg } from "@/components/Toast";
import { inviteError } from "@/lib/invites";
import { confirmThen } from "@/lib/confirm";
import { openSettingsTab } from "@/lib/nav";
import { RelayMembershipLine } from "@/components/RelayMembershipLine";
import { useRowMenu, type RowAction } from "@/components/RowMenu";
import { MoreMenu, type MoreMenuItem } from "@/components/MoreMenu";
import { Sheet } from "@/components/kit/Sheet";
import {
  Button,
  IconButton,
  Modal,
  Section,
  Field,
  SelectField,
  FormDisclosure,
  EmptyHint,
  ErrorState,
  LoadingState,
  Badge,
  StatusDot,
  type Tone,
  LinkButton,
  Hint,
  MetaTag,
  InfoTip,
  RowList,
  ListRow,
} from "@/components/ui";
import { Markdown } from "@/components/Markdown";
import { Menu } from "@/components/Menu";
import {
  IconX,
  IconChevronDown,
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
import { LIKELY_ME_HINT, likelyMeReason } from "@/lib/likelyMe";
import { openArchivedProposals, requestDevices } from "@/lib/nav";

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
  primary,
  more,
}: {
  title: string;
  /// What the pane is for, in a sentence. Not shown on screen: it's the "?"
  /// link's tooltip and accessible name, so the pane opens on its content.
  about: string;
  /// The docs page the "?" link opens (a path under the docs site).
  docs: string;
  children: ReactNode;
  /// Optional header controls, on the title row left of the close button.
  actions?: ReactNode;
  /// The pane's one primary action ("Invite", "Add skill"), in the header.
  primary?: { label: string; onClick: () => void; disabled?: boolean };
  /// Secondary and rare pane actions, behind the header's "⋯".
  more?: MoreMenuItem[];
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
          {(actions || primary || (more && more.length > 0)) && (
            <div className="ml-auto flex items-center gap-1.5">
              {actions}
              {primary && (
                <Button size="sm" disabled={primary.disabled} onClick={primary.onClick}>
                  {primary.label}
                </Button>
              )}
              {more && more.length > 0 && <MoreMenu label={`More ${title} actions`} items={more} />}
            </div>
          )}
        </div>
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
// One view, no scope toggle: this chat's runtime (genuinely per-chat), then
// the workspace's agents and MCP servers, tagged "workspace" and edited in
// place through their row menus (§11.1: the workspace owns configuration).


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

  return (
    <RailFrame
      title="Tools"
      about="What the agents here can reach: this chat's runtime, the workspace's agents and its MCP servers."
      docs={PANE_DOCS.tools}
      primary={{ label: "Add an agent", onClick: () => setShowAddAgent(true) }}
      more={[{ label: "Install an MCP server…", onSelect: () => setShowInstallMcp(true) }]}
    >
      <Section title="Runtime" info="The runtime that answers untagged messages in this chat. Pick one here; it applies to this chat only. Manage runtimes in Settings → Models.">
        {runtimeList.length === 0 ? (
          <EmptyHint text="No runtimes configured" action={<SetupAction flow="agent" />} />
        ) : (
          <RuntimeSwitcher runtimes={runtimeList} activeId={activeRuntimeId} onSelect={(id) => void selectRuntime(id)} />
        )}
      </Section>

      <Section
        title="Agents"
        info="Agents you can @mention. The roster belongs to the workspace: changes here apply in every chat. Each agent runs on a host: this device, or a worker (a headless box you enroll under People)."
        tag={<MetaTag>workspace</MetaTag>}
      >
        <RowList label="Agents">
          {/* The default agent (@ligus) always exists: it answers untagged
              messages on this chat's runtime (F1). It has no roster record, so
              no menu. A roster agent with the same handle IS the default (a
              legacy entry), so it's shown once. */}
          {!agentList.some((a) => a.name.toLowerCase() === PRIMARY_HANDLE) && (
            <ListRow
              leading={
                <Avatar
                  name={PRIMARY_NAME}
                  kind="agent"
                  host={activeRuntime?.location?.trim().toLowerCase() === "remote" ? "remote" : "local"}
                  size={22}
                />
              }
              title={`@${PRIMARY_HANDLE}`}
              meta={activeRuntime ? runtimePickerLabel(activeRuntime) : "default runtime"}
              tip="The default agent: answers untagged messages on this chat's runtime"
            />
          )}
          {agentList.map((agent) => (
            <AgentRosterRow
              key={agent.id}
              sessionId={sessionId}
              agent={agent}
              hosts={hostList}
              runtimes={runtimeList}
              isDefault={agent.name.toLowerCase() === PRIMARY_HANDLE}
              onRemove={() =>
                void confirmDialog(agentRemovalConsequences(agent.name), {
                  title: `Remove @${agent.name}?`,
                  confirmLabel: "Remove agent",
                  danger: true,
                }).then((ok) => ok && removeAgentMutation.mutate(agent.id))
              }
              onChanged={() => qc.invalidateQueries({ queryKey: ["agents", sessionId] })}
            />
          ))}
        </RowList>
      </Section>

      <Section
        title="MCP servers"
        info="Servers stay inert until enabled. Only enabled servers give agents external tools. The list belongs to the workspace."
        tag={<MetaTag>workspace</MetaTag>}
      >
        {mcpList.length === 0 ? (
          <EmptyHint text="No MCP servers" action={<SetupAction flow="tool" />} />
        ) : (
          <RowList label="MCP servers">
            {mcpList.map((server) => (
              <McpServerCard
                key={server.id}
                server={server}
                editable
                onToggle={(v) => toggleMcp(server, v)}
                onRemove={
                  server.isManaged
                    ? () => confirmThen("Remove this MCP server?", () => removeMcpMutation.mutate(server.id))
                    : undefined
                }
              />
            ))}
          </RowList>
        )}
      </Section>

      {showAddAgent && (
        <Sheet title="Add an agent" side="center" width={420} onClose={() => setShowAddAgent(false)}>
              <div className="space-y-2">
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
              </div>
        </Sheet>
      )}
      {showInstallMcp && (
        <Sheet title="Install an MCP server" side="center" width={420} onClose={() => setShowInstallMcp(false)}>
              <div className="space-y-2">
                <input
                  value={mcpSource}
                  onChange={(event) => setMcpSource(event.target.value)}
                  placeholder="URL, GitHub blob URL or owner/repo/mcp.json"
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
              </div>
        </Sheet>
      )}
    </RailFrame>
  );
}

/// One MCP server row (§6.4): id, transport detail, a Switch (editable) or a
/// plain on/off (read-only); Remove lives in the row menu. The backend exposes
/// no per-tool policy or tool list, so the row's tooltip says so honestly.
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
  const disabled = !server.enabled;
  return (
    <ListRow
      title={server.id}
      meta={`${server.transport} · ${server.detail}`}
      tip={
        disabled
          ? "Disabled — inert gate: this server is never launched or connected."
          : "Per-tool permissions arrive with backend support."
      }
      actionsLabel={`Actions for ${server.id}`}
      actions={
        editable
          ? [
              { label: disabled ? "Enable" : "Disable", onSelect: () => onToggle?.(disabled) },
              ...(onRemove ? [{ label: "Remove server", danger: true, onSelect: onRemove }] : []),
            ]
          : undefined
      }
      trailing={
        <span
          className="inline-flex shrink-0 items-center gap-1 text-[11px]"
          style={{ color: disabled ? "var(--ligus-ink-soft)" : "var(--ligus-success-ink)" }}
        >
          <StatusDot on={!disabled} size={7} label={disabled ? "Off: never launched or connected" : "On"} />
          {disabled ? "off" : "on"}
        </span>
      }
    />
  );
}

/// The chat's runtime: the active one as a single row, the others behind one
/// "N more" disclosure (a click there switches this chat to it).
function RuntimeSwitcher({
  runtimes,
  activeId,
  onSelect,
}: {
  runtimes: RuntimeSummaryDto[];
  activeId: string;
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const active = runtimes.find((r) => r.id === activeId);
  const others = runtimes.filter((r) => r.id !== activeId);
  return (
    <>
      <RowList label="Runtime">
        {active ? (
          <RuntimeRow runtime={active} active />
        ) : (
          <ListRow title="Default runtime" meta="none selected" />
        )}
      </RowList>
      {others.length > 0 && (
        <FormDisclosure
          label={open ? "Hide other runtimes" : `${others.length} more`}
          open={open}
          onToggle={() => setOpen((v) => !v)}
        >
          <RowList label="Other runtimes">
            {others.map((rt) => (
              <RuntimeRow key={rt.id} runtime={rt} active={false} onSelect={() => onSelect(rt.id)} />
            ))}
          </RowList>
        </FormDisclosure>
      )}
    </>
  );
}

/// Where an agent runs, in words (for its row's tooltip).
function agentHostText(hostId: string, hosts: WorkspaceHostDto[]): string {
  if (!hostId) return "Runs on this device";
  const host = hosts.find((h) => h.id === hostId);
  const label = host?.label || `${hostId.slice(0, 8)}…`;
  return host?.online
    ? `Runs on worker “${label}”`
    : `Worker “${label}” is offline; queued work waits until it reconnects`;
}

/// An agent's host state, shown only when it matters: an offline worker gets
/// a labelled "offline". Everything else is in the row's tooltip.
function AgentHostState({ hostId, hosts }: { hostId: string; hosts: WorkspaceHostDto[] }) {
  if (!hostId) return null;
  const host = hosts.find((h) => h.id === hostId);
  if (host?.online) return null;
  const text = agentHostText(hostId, hosts);
  return (
    <span
      className="inline-flex shrink-0 items-center gap-1 text-[11px]"
      data-tip={text}
      style={{ color: "var(--ligus-warn-ink)" }}
    >
      <IconMonitor size={12} />
      offline
      <span className="sr-only">: {text}</span>
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
  runtimes,
  isDefault = false,
  onRemove,
  onChanged,
}: {
  sessionId: string;
  agent: import("@/lib/ipc").WorkspaceAgentDto;
  hosts: WorkspaceHostDto[];
  runtimes: RuntimeSummaryDto[];
  /// The roster entry that carries the default handle (@ligus).
  isDefault?: boolean;
  onRemove: () => void;
  onChanged: () => void;
}) {
  const [accessOpen, setAccessOpen] = useState(false);
  const avatarInput = useRef<HTMLInputElement>(null);
  const rm = useRowMenu(`More actions for @${agent.name}`, [
    ...(agent.canManageAccess ? [{ label: "Access…", onSelect: () => setAccessOpen(true) }] : []),
    { label: "Upload avatar…", onSelect: () => avatarInput.current?.click() },
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
  ]);
  return (
    <li className="px-1" {...rm.rowProps}>
      <div className="ligus-listrow flex items-center gap-2">
        <label className="group/av relative shrink-0 cursor-pointer" data-tip="Upload an avatar for this agent">
          <Avatar
            name={agent.name}
            url={agent.avatarUrl}
            colorHex={agent.avatarColorHex}
            kind="agent"
            size={22}
          />
          <span
            aria-hidden
            className="absolute inset-0 rounded-[30%] opacity-0 transition-opacity group-hover/av:opacity-100"
            style={{ background: "color-mix(in srgb, var(--ligus-ink) 35%, transparent)" }}
          />
          <input
            ref={avatarInput}
            type="file"
            accept="image/*"
            aria-label={`Upload an avatar for @${agent.name}`}
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
        <span
          className="flex min-w-0 flex-1 items-baseline gap-1.5"
          data-tip={[
            agent.role,
            `Access: ${agent.canManageAccess ? accessSummary(agent) : accessLabel(agent)}`,
            agentHostText(agent.hostId, hosts),
          ]
            .filter(Boolean)
            .join(" · ")}
        >
          <span className="shrink-0 truncate text-[13px] font-medium">@{agent.name}</span>
          <span className="min-w-0 flex-1 truncate text-[11.5px]" style={{ color: "var(--ligus-ink-soft)" }}>
            {agentModelLabel(agent.runtimeId, runtimes)}
            {isDefault ? " · default" : ""}
          </span>
        </span>
        <AgentHostState hostId={agent.hostId} hosts={hosts} />
        {rm.button}
      </div>
      {rm.menu}
      {accessOpen && (
        <Sheet title={`Access for @${agent.name}`} side="center" width={440} onClose={() => setAccessOpen(false)}>
          <AgentAccessEditor sessionId={sessionId} agent={agent} />
        </Sheet>
      )}
    </li>
  );
}

function RuntimeRow({
  runtime,
  active,
  onSelect,
}: {
  runtime: RuntimeSummaryDto;
  active: boolean;
  onSelect?: () => void;
}) {
  return (
    <ListRow
      title={runtime.label}
      meta={`${runtime.location} · ${runtime.provider}`}
      tip={[
        runtime.endpoint,
        runtime.isManaged ? "A runtime you configured (vs. an auto-detected local CLI)" : null,
      ]
        .filter(Boolean)
        .join(" · ") || undefined}
      onClick={onSelect}
      selected={active}
      trailing={
        active ? (
          <span className="text-[11px] font-semibold" style={{ color: "var(--ligus-success-ink)" }}>
            active
          </span>
        ) : undefined
      }
    />
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
    <ListRow
      title={`@${item.agentName}`}
      meta={`in ${item.chatTitle || "Untitled chat"}`}
      trailing={
        <>
          <HostStatusBadge item={item} />
          {needsHandoff && (
            <RunOnWorkerMenu
              workers={workers}
              onReassign={(hostId) => onReassign(item.agentId, hostId)}
            />
          )}
        </>
      }
    />
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
  // Nothing queued: no section at all, rather than an empty box.
  if (items.length === 0) return null;
  return (
    <Section title="Queued work">
      <RowList label="Queued work">
        {items.map((item) => (
          <QueuedWorkRow
            key={`${item.sessionId}:${item.agentId}`}
            item={item}
            workers={workers}
            onReassign={(agentId, hostId) => reassign.mutate({ agentId, hostId })}
          />
        ))}
      </RowList>
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
  onArchive,
  onHide,
  defaultOpen,
}: {
  proposal: ProposalDto;
  sessionId: string;
  onChanged: () => void;
  /// Archive for everyone (true) or restore to the inbox (false).
  onArchive: (archived: boolean) => void;
  /// Hide for me on this device (true) or show again (false).
  onHide: (hidden: boolean) => void;
  /// Open the detail on mount. Rows start as one line; the first active
  /// proposal opens (you're reviewing it), the rest and settled ones don't.
  defaultOpen?: boolean;
}) {
  const qc = useQueryClient();
  const settled = isSettled(proposal);
  const [expanded, setExpanded] = useState(defaultOpen ?? !settled);
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
  const requestApply = useMutation({
    mutationFn: (hostId: string) => requestApplyOnHost(sessionId, proposal.id, hostId),
    onSuccess: (_r, hostId) => {
      toast.success(`Asked ${applyHosts.find((h) => h.id === hostId)?.label ?? "the host"} to apply it.`);
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
  const cantImplement = implementRefusal(proposal);
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

  const rowMenu = useRowMenu(`Actions for ${proposal.title}`, [
    ...(proposal.diff ? [{ label: "View full diff", onSelect: () => setDiffOpen(true) }] : []),
    ...(mine !== null && isVotable(proposal)
      ? [{ label: mine ? "Change vote to reject" : "Change vote to approve", onSelect: () => vote.mutate(!mine), disabled: busy }]
      : []),
    ...(proposal.canRequestApply
      ? applyHosts.map((h) => ({
          label: `Apply on ${h.label}${h.online ? "" : " (offline)"}`,
          title: "That host applies it once, on its checkout, and reports the result. Nobody else applies it meanwhile.",
          disabled: busy,
          onSelect: () => requestApply.mutate(h.id),
        }))
      : []),
    ...(proposal.canArchive
      ? [{ label: "Archive", title: "Takes it out of the inbox for everyone. Restore it from Archived proposals.", onSelect: () => onArchive(true) }]
      : []),
    ...(proposal.archived ? [{ label: "Restore", title: "Back in the inbox for everyone", onSelect: () => onArchive(false) }] : []),
    proposal.hidden
      ? { label: "Show again", title: "Back in your inbox on this device", onSelect: () => onHide(false) }
      : { label: "Hide for me", title: "Only on this device. Others still see it.", onSelect: () => onHide(true) },
  ]);
  return (
    <li className="px-1 py-2" {...rowMenu.rowProps} data-testid="proposal-row">
      <div className="flex items-center justify-between gap-2">
        <button
          type="button"
          className="-mx-1 flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-1 py-0.5 text-left transition-colors enabled:hover:bg-[color:var(--ligus-hover)]"
          onClick={() => hasDetail && setExpanded((v) => !v)}
          aria-expanded={hasDetail ? expanded : undefined}
          disabled={!hasDetail}
        >
          {hasDetail && (
            <span
              aria-hidden
              className="shrink-0 transition-transform"
              style={{ transform: expanded ? "rotate(0deg)" : "rotate(-90deg)", opacity: 0.5 }}
            >
              <IconChevronDown size={13} />
            </span>
          )}
          <span className="min-w-0 truncate text-[13px] font-medium">{proposal.title}</span>
          <span className="shrink-0 text-[11.5px] text-ink-soft">{relTime(proposal.createdAt)}</span>
        </button>
        <div className="flex shrink-0 items-center gap-1.5">
          <span
            className="text-[11px] text-ink-soft"
            data-tip={`${proposal.qualifyingApprovals} of ${proposal.requiredApprovals} approvals needed`}
          >
            {proposal.qualifyingApprovals}/{proposal.requiredApprovals}
          </span>
          <StatusPill status={proposal.status} />
          {rowMenu.button}
        </div>
      </div>
      {rowMenu.menu}
      {expanded && <ProposalBadges proposal={proposal} />}

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
                Approving adds it to Workflows. It runs only when someone presses Run.
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

      {/* Who voted: one compact chip per voter (role in the tooltip). */}
      {proposal.approvals.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1" aria-label="Votes">
          {proposal.approvals.map((a) => (
            <span
              key={a.actorId}
              className="inline-flex items-center gap-1 rounded-full border px-1.5 text-[11px] leading-5"
              style={{ borderColor: "var(--ligus-line)" }}
              data-tip={`${a.displayName} ${a.approved ? "approved" : "rejected"} · ${a.role}`}
            >
              <span
                aria-hidden
                style={{ color: a.approved ? "var(--ligus-success-ink)" : "var(--ligus-danger-ink)" }}
              >
                {a.approved ? "✓" : "✕"}
              </span>
              <span className="font-medium">{a.displayName}</span>
              <span className="sr-only">{a.approved ? "approved" : "rejected"} · {a.role}</span>
            </span>
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
        {proposal.quorumMet && proposal.status !== "applied" && !cantImplement && (
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
          {cantImplement ??
            "Quorum met"}
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
    </li>
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
  // Settled work is history, so it starts collapsed. Archived and hidden
  // proposals leave the pane for the Archived proposals view (⋯); nothing is
  // deleted.
  const [showSettled, setShowSettled] = useState(false);

  // Alternatives of a proposal set render inside their set, not on their own.
  const sets = useProposalSets(sessionId);
  const groups = groupProposals(withoutSetAlternatives(proposals.data ?? [], sets.data ?? []));
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    qc.invalidateQueries({ queryKey: ["chat", sessionId] });
  };
  const afterShelving = () => {
    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    qc.invalidateQueries({ queryKey: ["archived-proposals"] });
    qc.invalidateQueries({ queryKey: ["chats", "attention"] });
  };
  const archive = async (ids: string[], archived: boolean) => {
    try {
      const n = await archiveProposals(sessionId, ids, archived);
      afterShelving();
      if (archived && n > 0) toast.success(n === 1 ? "Archived." : `${n} archived.`);
    } catch (e) {
      toast.error(`Couldn't ${archived ? "archive" : "restore"}: ${errMsg(e)}`);
    }
  };
  const hide = async (ids: string[], hidden: boolean) => {
    try {
      await hideProposals(ids, hidden);
      afterShelving();
    } catch (e) {
      toast.error(`Couldn't ${hidden ? "hide" : "show"} it: ${errMsg(e)}`);
    }
  };
  const card = (proposal: ProposalDto, i = 1) => (
    <ProposalCard
      key={proposal.id}
      proposal={proposal}
      sessionId={sessionId}
      onChanged={refresh}
      onArchive={(archived) => archive([proposal.id], archived)}
      onHide={(hidden) => hide([proposal.id], hidden)}
      defaultOpen={!isSettled(proposal) && i === 0}
    />
  );

  const loading = proposals.isPending || sets.isPending;
  const failed = proposals.isError || sets.isError;
  return (
    <RailFrame
      title="Review"
      about="Pending decisions and proposal quorum for this chat."
      docs={PANE_DOCS.review}
      more={[
        {
          label: "Archived proposals",
          onSelect: openArchivedProposals,
        },
      ]}
    >
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
        {groups.active.length > 0 && <RowList label="Pending proposals">{groups.active.map((p, i) => card(p, i))}</RowList>}
        {groups.active.length === 0 && (
          <EmptyHint icon={<IconInbox size={18} />} text="Nothing to review" />
        )}
      </Section>

      {groups.settled.length > 0 && (
        <Section
          title="Settled"
          action={
            <button
              className="text-xs text-ink-soft transition-colors hover:text-ink"
              data-tip="Archives them for everyone"
              onClick={() => archive(groups.settled.filter((p) => p.canArchive).map((p) => p.id), true)}
            >
              Archive settled
            </button>
          }
        >
          <FormDisclosure
            label={`${groups.settled.length} settled proposal${groups.settled.length === 1 ? "" : "s"}`}
            open={showSettled}
            onToggle={() => setShowSettled((v) => !v)}
          >
            <RowList label="Settled proposals">{groups.settled.map((p) => card(p))}</RowList>
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
  const profile = useQuery({ queryKey: ["my-profile"], queryFn: myProfile });
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
  const [inviteOpen, setInviteOpen] = useState(false);
  const [inviteOptions, setInviteOptions] = useState(false);
  const [sheet, setSheet] = useState<"history" | "agent" | null>(null);
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
  const relayMembership = useQuery({
    queryKey: ["server-members"],
    queryFn: relayMembershipStatus,
    enabled: relayOn,
  });
  const audit = useQuery({ queryKey: ["membership-audit", sessionId], queryFn: listMembershipAudit });
  const refreshSrv = () => qc.invalidateQueries({ queryKey: ["server-members"] });
  const claimMutation = useMutation({
    mutationFn: workspaceClaimMembership,
    onSuccess: (claimed) => {
      refreshSrv();
      if (claimed) {
        toast.success("Claimed. You're the owner on the relay, and it now refuses devices whose token names no member.");
      } else {
        toast.error("Not claimed: this relay has membership turned off, or someone already claimed this workspace.");
      }
    },
    onError: (e) => toast.error(`Couldn't claim: ${errMsg(e)}`),
  });
  const claim = () =>
    confirmThen(
      "Claim this workspace on the relay? You become its owner there, and the relay then refuses reads and writes from every device whose relay token doesn't name a member, including your other devices and boxes until you add them. Ligus can't undo a claim.",
      () => claimMutation.mutate(),
    );
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

  // Relay (server-enforced) membership merged into the member rows: a relay
  // entry that maps to a workspace member marks that row; the rest are
  // "Relay only".
  const roster = members.data ?? [];
  const relayByMember = new Map<string, ServerMember>();
  const relayOnly: ServerMember[] = [];
  for (const m of relayMembership.data?.members ?? []) {
    const hit = relayMemberFor(m, profile.data?.accountId, roster);
    if (hit) relayByMember.set(hit.id, m);
    else relayOnly.push(m);
  }
  const removeFromRelay = (m: ServerMember, name: string) =>
    confirmThen(
      `Remove ${name} from this workspace? They're removed on the relay and from the workspace, then the key is rotated so they can't read new messages.`,
      () => srvRemoveMutation.mutate(m.account),
    );
  const relayState = relayMembership.data?.state;
  const relayEnforced = relayOn && (relayState === "claimed" || relayState === "hidden");
  // "Is this you on another computer?": only where Add a computer works (a
  // team workspace, a desktop holding your account key).
  const canLinkComputers = Boolean(activeRoom && profile.data?.safetyCode);
  const openInvites = (invites.data ?? []).filter((i) => !i.revoked);

  return (
    <RailFrame
      title="People"
      about="Workspace members and governance roles for this chat."
      docs={PANE_DOCS.people}
      primary={{ label: "Invite", onClick: () => setInviteOpen(true) }}
      more={[
        { label: "Membership history", onSelect: () => setSheet("history") },
        ...(relayOn ? [{ label: "Add a remote agent…", onSelect: () => setSheet("agent") }] : []),
      ]}
    >
      <Section
        title="Members"
        tag={relayEnforced ? <MetaTag>relay enforced</MetaTag> : undefined}
      >
        {error && (
          <div className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
            {error}
          </div>
        )}
        {!selfMember && (
          <div className="rounded-xl border px-3 py-2 text-xs" style={{ borderColor: "var(--ligus-line)", color: "var(--ligus-warm-ink)" }}>
            You aren't in this workspace's roster — it was created under a different identity. If it's
            your own, re-create it; identity is stable going forward.
          </div>
        )}
        {relayOn && relayMembership.data && (
          <RelayMembershipLine
            status={relayMembership.data}
            claiming={claimMutation.isPending}
            onClaim={claim}
            onToken={() => openSettingsTab("team-sync")}
          />
        )}
        <RowList label="Members">
          {selfMember && (
            <ListRow
              leading={<Avatar name={selfMember.displayName} url={selfMember.avatarUrl} kind="human" size={22} />}
              title={selfMember.displayName}
              meta={`you · ${selfMember.role}`}
              trailing={relayByMember.has(selfMember.id) ? <RelayMark role={relayByMember.get(selfMember.id)!.role} /> : undefined}
            />
          )}
          {collaborators.map((member) => {
            const relay = relayByMember.get(member.id);
            return (
              <MemberRow
                key={member.id}
                member={member}
                online={onlineActors.has(member.actorId)}
                presenceKnown={relayOn}
                dup={(nameCounts.get(member.displayName) ?? 0) > 1 && member.index > 0}
                relayRole={relay?.role}
                onRole={(role) => setRoleMutation.mutate({ id: member.id, role })}
                onRemove={() =>
                  confirmThen("Remove this member from the roster?", () => removeMutation.mutate(member.id))
                }
                onRegrant={() => regrantMutation.mutate(member.id)}
                onRevoke={() =>
                  confirmThen(
                    `Remove ${member.displayName} and revoke their access? The workspace key will be rotated so they can't read new messages.`,
                    () => revokeMutation.mutate(member.id),
                  )
                }
                onRemoveFromRelay={relay ? () => removeFromRelay(relay, member.displayName) : undefined}
                onLinkComputer={
                  canLinkComputers && selfMember && likelyMeReason(member, selfMember.displayName, hosts.data ?? [])
                    ? () => requestDevices({ open: "computer", member: { memberId: member.id, displayName: member.displayName } })
                    : undefined
                }
              />
            );
          })}
        </RowList>
        {collaborators.length === 0 && <EmptyHint text="No other members yet" />}
      </Section>

      {relayOnly.length > 0 && (
        <Section
          title="Relay only"
          info="Accounts the relay accepts writes from that match no member here (for example a box that never joined)."
        >
          <RowList label="Relay only">
            {relayOnly.map((m) => {
              const name = relayMemberName(m, profile.data?.accountId, roster);
              return (
                <ListRow
                  key={m.account}
                  title={name}
                  meta={m.role}
                  tip={m.account}
                  actionsLabel={`Actions for ${name}`}
                  actions={[
                    { label: "Copy account id", onSelect: () => void navigator.clipboard.writeText(m.account).catch(() => {}) },
                    { label: "Remove from relay…", danger: true, onSelect: () => removeFromRelay(m, name) },
                  ]}
                />
              );
            })}
          </RowList>
        </Section>
      )}

      {relayOn && openInvites.length > 0 && (
        <Section title="Open invites">
          <RowList label="Open invites">
            {openInvites.map((i: InviteEntry) => (
              <ListRow
                key={i.id}
                title={i.role}
                meta={`${i.maxUses === 0 ? `${i.uses} uses` : `${i.uses}/${i.maxUses} used`}${
                  i.expiresAt > 0 ? ` · expires ${new Date(i.expiresAt * 1000).toLocaleDateString()}` : ""
                }`}
                tip="The code itself was shown once, when it was created."
                actionsLabel={`Actions for the ${i.role} invite`}
                actions={[{ label: "Revoke invite", danger: true, onSelect: () => revokeInviteMutation.mutate(i.id) }]}
              />
            ))}
          </RowList>
        </Section>
      )}

      <Section title="Workers & hosts">
        {(hosts.data ?? []).length === 0 ? (
          <EmptyHint text="No hosts yet" action={<SetupAction flow="box" />} />
        ) : (
          <RowList label="Workers and hosts">
            {(hosts.data ?? []).map((host) => <HostRow key={host.id} host={host} />)}
          </RowList>
        )}
      </Section>

      {inviteOpen && (
        <Sheet
          title="Invite someone"
          side="center"
          width={420}
          onClose={() => setInviteOpen(false)}
          footer={<Button onClick={() => setInviteOpen(false)}>Done</Button>}
        >
          <div className="space-y-3">
          <div className="flex items-center gap-1.5">
            <Hint className="px-0">Single-use. Send it over a channel you trust.</Hint>
            <InfoTip
              label="invite links"
              text="The link carries your safety code so they can check it came from you. To invite someone you already share another workspace with, use People."
            />
          </div>
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
          {relayOn && (
            <FormDisclosure label="Options: a relay invite code" open={inviteOptions} onToggle={() => setInviteOptions((v) => !v)}>
              <div className="space-y-2">
                <div className="flex items-center gap-1.5">
                  <Hint className="px-0">Joins at a set role; revocable.</Hint>
                  <InfoTip
                    label="relay invite codes"
                    text="Share a code so someone can join at a set role. Codes are revocable, can expire, and can cap how many times they're used. 0 days = never expires; 0 uses = unlimited. The code shows once, on creation."
                  />
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  <select
                    value={invRole}
                    onChange={(e) => setInvRole(e.target.value)}
                    aria-label="Role"
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
                  <Button disabled={createInviteMutation.isPending} onClick={() => createInviteMutation.mutate()}>
                    Create code
                  </Button>
                </div>
                {lastCode && (
                  <>
                    <Hint className="px-0">Shown once. Copy it now.</Hint>
                    <div
                      className="flex items-center justify-between gap-2 rounded-lg border px-2.5 py-1.5"
                      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
                    >
                      <code className="truncate text-xs">{lastCode}</code>
                      <button className="shrink-0 text-xs underline hover:opacity-80" onClick={() => void navigator.clipboard.writeText(lastCode).catch(() => {})}>
                        Copy
                      </button>
                    </div>
                  </>
                )}
              </div>
            </FormDisclosure>
          )}
          </div>
        </Sheet>
      )}

      {sheet === "history" && (
        <Sheet
          title="Membership history"
          description="Signed, tamper-evident log"
          side="center"
          width={460}
          onClose={() => setSheet(null)}
        >
          {(audit.data ?? []).length === 0 ? (
            <EmptyHint text="No changes yet" />
          ) : (
            <RowList label="Membership changes">
              {(audit.data ?? []).map((e, i) => (
                <li key={i} className="ligus-listrow flex items-baseline justify-between gap-2 px-1 text-xs">
                  <span className="min-w-0 truncate">
                    <span style={{ color: e.action === "removed" ? "var(--ligus-danger-ink)" : "var(--ligus-accent-warm)" }}>
                      {e.action === "removed" ? "removed" : `added${e.role ? ` (${e.role})` : ""}`}
                    </span>{" "}
                    <b>{e.member}</b> <span className="text-ink-soft">by {e.by}</span>
                  </span>
                  <span className="shrink-0 text-ink-soft" data-tip={e.at}>
                    {new Date(e.at).toLocaleDateString()}
                  </span>
                </li>
              ))}
            </RowList>
          )}
        </Sheet>
      )}

      {sheet === "agent" && (
        <Sheet title="Add a remote agent" side="center" width={460} onClose={() => setSheet(null)}>
          <div className="space-y-2">
          <div className="flex items-center gap-1.5">
            <Hint className="px-0">A one-paste command for a headless box.</Hint>
            <InfoTip
              label="remote agents"
              text="Generate makes the one-paste command a developer runs on its box (installs the CLI, connects to this workspace, starts the worker). Your relay issuer key mints the agent's identity token locally; only the token travels to the box."
            />
          </div>
          {!issuerKey.data ? (
            <Hint className="px-0">Add your relay issuer key in Settings → Team sync</Hint>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-2">
                <select
                  value={agentRole}
                  onChange={(e) => setAgentRole(e.target.value)}
                  aria-label="Agent role"
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
                  className="w-36 rounded-lg border px-2.5 py-1.5 text-sm"
                  style={fieldStyle}
                />
                <Button variant="primary" disabled={bootstrapMutation.isPending} onClick={() => bootstrapMutation.mutate()}>
                  Generate
                </Button>
              </div>
              {agentBlock && (
                <div>
                  <textarea
                    readOnly
                    value={agentBlock}
                    spellCheck={false}
                    aria-label="Bootstrap command"
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
          </div>
        </Sheet>
      )}
    </RailFrame>
  );
}

type ServerMember = import("@/lib/ipc").MemberEntry;

/// The workspace member a relay entry belongs to: your own account, or the
/// member whose actor id is the relay account (with or without "ligus:").
function relayMemberFor<T extends { id: string; actorId: string; isSelf?: boolean }>(
  m: { account: string },
  myAccountId: string | undefined,
  roster: T[],
): T | undefined {
  const id = m.account.replace(/^ligus:/, "");
  if (myAccountId && id === myAccountId) return roster.find((r) => r.isSelf);
  return roster.find((r) => r.actorId === id || r.actorId === m.account);
}

/// A relay member's readable name: their forge login when the relay has one,
/// else the matching member, else a short account id. Never a bare "@".
function relayMemberName(
  m: { account: string; login: string },
  myAccountId: string | undefined,
  roster: { id: string; actorId: string; displayName: string; isSelf?: boolean }[],
): string {
  if (m.login) return `@${m.login}`;
  const hit = relayMemberFor(m, myAccountId, roster);
  if (hit) return hit.isSelf ? "you" : hit.displayName;
  return `account ${m.account.replace(/^ligus:/, "").slice(0, 8)}`;
}

/// A small mark on a member row: the relay also enforces this member.
function RelayMark({ role }: { role: string }) {
  return (
    <span
      className="shrink-0 text-[11px]"
      style={{ color: "var(--ligus-ink-soft)" }}
      data-tip={`The relay accepts writes from them (${role})`}
      aria-label={`On the relay as ${role}`}
      role="img"
    >
      relay
    </span>
  );
}

export type RosterMember = import("@/bindings/WorkspaceMemberDto").WorkspaceMemberDto & { index: number };

/// One workspace member as a single row; role changes and removal live in
/// the row menu (right-click, the hover "⋯", or Shift+F10).
export function MemberRow({
  member,
  online,
  presenceKnown,
  dup,
  relayRole,
  onRole,
  onRemove,
  onRegrant,
  onRevoke,
  onRemoveFromRelay,
  onLinkComputer,
}: {
  member: RosterMember;
  online: boolean;
  presenceKnown: boolean;
  dup: boolean;
  /// Set when the relay also enforces this member.
  relayRole?: string;
  onRole: (role: string) => void;
  onRemove: () => void;
  onRegrant: () => void;
  onRevoke: () => void;
  onRemoveFromRelay?: () => void;
  /// Set when this member looks like you on another computer (lib/likelyMe):
  /// offer Add a computer, never act.
  onLinkComputer?: () => void;
}) {
  // The backend computes each refusal with the fold's own rules (an older
  // backend sends none: everything stays offered).
  const refusalOf = (r: string) => member.roleOptions?.find((o) => o.role === r)?.refusal ?? null;
  const roleActions: RowAction[] = MEMBER_ROLES.filter((r) => r !== member.role).map((r) => ({
    label: `Change role to ${r}`,
    onSelect: () => onRole(r),
    disabled: Boolean(refusalOf(r)),
    title: refusalOf(r) ?? undefined,
  }));
  const rm = useRowMenu(`Actions for ${member.displayName}`, [
    ...(onLinkComputer
      ? [
          {
            label: `${LIKELY_ME_HINT}…`,
            onSelect: onLinkComputer,
            title: "Opens Add a computer: make it one of your devices, then remove this member",
          },
        ]
      : []),
    ...roleActions,
    {
      label: "Allow new passkey",
      onSelect: onRegrant,
      disabled: Boolean(member.regrantRefusal),
      title:
        member.regrantRefusal ??
        "Under rules v2, let them register a first passkey (after theirs were removed). Confirmed with your passkey.",
    },
    {
      label: "Remove",
      onSelect: onRemove,
      danger: true,
      disabled: Boolean(member.removeRefusal),
      title: member.removeRefusal ?? "Remove from the member list only",
    },
    {
      label: "Remove & revoke",
      onSelect: onRevoke,
      danger: true,
      disabled: Boolean(member.removeRefusal),
      title: member.removeRefusal ?? "Remove and rotate the key so they lose access (for a bad actor)",
    },
    ...(onRemoveFromRelay
      ? [
          {
            label: "Remove from relay…",
            onSelect: onRemoveFromRelay,
            danger: true,
            title: "Remove them on the relay and from the workspace, then rotate the key",
          },
        ]
      : []),
  ]);
  return (
    <li className="px-1" {...rm.rowProps} data-testid={`member-${member.id}`}>
      <div className="ligus-listrow flex items-center gap-2" data-tip={onLinkComputer ? LIKELY_ME_HINT : undefined}>
        <span className="relative shrink-0">
          <Avatar name={member.displayName} url={member.avatarUrl} kind="human" size={22} />
          <span className="absolute -bottom-0.5 -right-0.5 inline-flex rounded-full" style={{ background: "var(--ligus-panel)" }}>
            <StatusDot on={online} size={8} label={presenceKnown ? (online ? "Online" : "Offline") : "Presence needs a relay"} />
          </span>
        </span>
        <span className="flex min-w-0 flex-1 items-baseline gap-1.5">
          <span className="shrink-0 truncate text-[13px] font-medium">{member.displayName}</span>
          {dup && (
            <span className="shrink-0 text-xs text-ink-soft" data-tip={`Mention precisely with @${member.displayName}#${member.index}`}>
              #{member.index}
            </span>
          )}
          <span className="min-w-0 flex-1 truncate text-[11.5px] text-ink-soft">
            {member.title ? `${member.title} · ${member.role}` : member.role}
            {onLinkComputer && <span data-testid="likely-me"> · you on another computer?</span>}
          </span>
        </span>
        {member.isFounder && <Badge title="The workspace's founder: nobody can remove or demote them">founder</Badge>}
        {member.kind === "service" && (
          <Badge title="A box (ligus serve or a worker): its own member, at most Contributor">service</Badge>
        )}
        {relayRole && <RelayMark role={relayRole} />}
        {rm.button}
      </div>
      {rm.menu}
    </li>
  );
}

/// A single registered device/worker: label, a kind chip, an online/offline
/// status dot, and a relative "last seen".
function HostRow({ host }: { host: WorkspaceHostDto }) {
  const gateways = hostGatewaysLabel(host);
  return (
    <ListRow
      leading={<StatusDot on={host.online} size={8} label={host.online ? "Online" : "Offline"} />}
      title={host.label || "Unnamed host"}
      meta={`${host.kind} · ${host.online ? "online" : relativeAgo(host.lastSeen)}`}
      tip={gateways ? `Gateways: ${gateways}` : undefined}
    />
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
          No agents yet: applies to all.
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
    <RailFrame
      title="Vaults"
      about="Reference material mounted into this workspace."
      docs={PANE_DOCS.vaults}
      primary={{ label: "Add vault", onClick: () => setShowAdd(true) }}
    >
      <Section title="Sources">
        {(vaults.data ?? []).length > 0 && (
          <RowList label="Vaults">
            {(vaults.data ?? []).map((vault) => (
              <ListRow
                key={vault.url}
                title={vault.label}
                meta={`${vault.kind} · ${targetLabel(vault.agentIds, agents.data ?? [])}`}
                tip={vault.url}
                onClick={() =>
                  preview?.url === vault.url ? setPreview(null) : previewVaultMutation.mutate(vault.url)
                }
                selected={preview?.url === vault.url}
                actionsLabel={`Actions for ${vault.label}`}
                actions={[
                  { label: "Preview", onSelect: () => previewVaultMutation.mutate(vault.url) },
                  {
                    label: "Remove vault…",
                    danger: true,
                    onSelect: () => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url)),
                  },
                ]}
              >
                {preview?.url === vault.url ? (
                  <pre
                    className="max-h-56 overflow-auto rounded-xl p-3 text-xs leading-5"
                    style={{ background: "var(--ligus-mist)" }}
                  >
                    {preview.text}
                  </pre>
                ) : undefined}
              </ListRow>
            ))}
          </RowList>
        )}
        {(vaults.data ?? []).length === 0 && (
          <EmptyHint text="No vaults mounted" />
        )}
        {showAdd && (
        <Sheet title="Add a vault" side="center" width={420} onClose={() => setShowAdd(false)}>
          <div className="space-y-2">
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
          </div>
        </Sheet>
        )}
      </Section>
    </RailFrame>
  );
}

function SkillsPane({ sessionId }: { sessionId: string }) {
  const [openSkill, setOpenSkill] = useState<string | null>(null);
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
    <RailFrame
      title="Skills"
      about="Instruction bundles injected into the active participants."
      docs={PANE_DOCS.skills}
      primary={{ label: "Install skill", onClick: () => setShowAdd(true) }}
    >
      <Section title="Loaded">
        {(skills.data ?? []).length > 0 && (
          <RowList label="Skills">
            {(skills.data ?? []).map((skill) => (
              <ListRow
                key={skill.id}
                title={skill.name}
                meta={targetLabel(skill.agentIds, agents.data ?? [])}
                tip={skill.sourceUrl || undefined}
                onClick={() => setOpenSkill(openSkill === skill.id ? null : skill.id)}
                selected={openSkill === skill.id}
                actionsLabel={`Actions for ${skill.name}`}
                actions={[
                  {
                    label: "Remove skill…",
                    danger: true,
                    onSelect: () => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id)),
                  },
                ]}
              >
                {openSkill === skill.id ? (
                  <div className="whitespace-pre-wrap px-1 text-xs leading-5 text-ink-soft">{skill.instructions}</div>
                ) : undefined}
              </ListRow>
            ))}
          </RowList>
        )}
        {(skills.data ?? []).length === 0 && <EmptyHint text="No skills loaded" />}
        {showAdd && (
        <Sheet title="Install a skill" side="center" width={420} onClose={() => setShowAdd(false)}>
          <div className="space-y-2">
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
              placeholder="URL, GitHub blob URL or owner/repo/SKILL.md"
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
          </div>
        </Sheet>
        )}
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

/// An agent's model as its row shows it ("claude-code / opus"), falling back
/// to the runtime id when the runtime isn't known here.
function agentModelLabel(runtimeId: string, runtimes: RuntimeSummaryDto[]): string {
  const rt = runtimes.find((r) => r.id === runtimeId);
  return rt ? runtimePickerLabel(rt) : runtimeId;
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
        <EmptyHint text="Context unavailable right now" />
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
              Context is getting full. Remove unused skills or vaults.
            </div>
          )}
        </div>
        <Hint className="mt-1.5">
          {data?.messageCount ?? 0} messages · {data?.skillCount ?? 0} skills · {data?.vaultCount ?? 0} vaults
        </Hint>
      </Section>

      <Section title="Skills">
        {(skills.data ?? []).length === 0 ? (
          <EmptyHint text="No skills loaded" />
        ) : (
          <RowList label="Injected skills">
            {(skills.data ?? []).map((skill) => (
              <ListRow
                key={skill.id}
                title={skill.name}
                meta={`~${fmtTokens(Math.ceil(skill.instructions.length / 4))} tokens`}
                actionsLabel={`Actions for ${skill.name}`}
                actions={[
                  {
                    label: "Remove skill…",
                    danger: true,
                    onSelect: () => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id)),
                  },
                ]}
              />
            ))}
          </RowList>
        )}
      </Section>

      <Section title="Vaults">
        {(vaults.data ?? []).length === 0 ? (
          <EmptyHint text="No vaults mounted" />
        ) : (
          <RowList label="Mounted vaults">
            {(vaults.data ?? []).map((vault) => (
              <ListRow
                key={vault.url}
                title={vault.label}
                meta={vault.kind}
                tip={vault.url}
                actionsLabel={`Actions for ${vault.label}`}
                actions={[
                  {
                    label: "Unmount vault…",
                    danger: true,
                    onSelect: () => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url)),
                  },
                ]}
              />
            ))}
          </RowList>
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
