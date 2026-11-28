import { useMemo, useState } from "react";
import type { AppearanceMode, ThemeName } from "@/lib/theme";
import { IconButton, Modal, NavRow, SectionCap } from "@/components/ui";
import {
  IconUsers,
  IconSparkle,
  IconFile,
  IconActivity,
  IconHexagon,
  IconWrench,
  IconLock,
  IconAlertTriangle,
  IconArrowDown,
  IconX,
} from "@/lib/icons";
import { AccountSection } from "@/components/settings/AccountSection";
import { AppearanceSection } from "@/components/settings/AppearanceSection";
import { FolderGitSection } from "@/components/settings/FolderGitSection";
import { RepositoriesSection } from "@/components/settings/RepositoriesSection";
import { CodeHostsSection } from "@/components/settings/CodeHostsSection";
import { TeamSyncSection } from "@/components/settings/TeamSyncSection";
import { SchedulesSection } from "@/components/settings/SchedulesSection";
import { ModelsSection } from "@/components/settings/ModelsSection";
import { ToolsSection } from "@/components/settings/ToolsSection";
import { PermissionsSection } from "@/components/settings/PermissionsSection";
import { UpdatesSection } from "@/components/settings/UpdatesSection";
import { DiagnosticsSection } from "@/components/settings/DiagnosticsSection";
import { DangerZoneSection } from "@/components/settings/DangerZoneSection";
import { GuardsSection } from "@/components/guards/GuardsSection";
import { FlowSection } from "@/components/settings/FlowSection";
import { SecuritySection } from "@/components/settings/SecuritySection";
import { WorkspaceRulesSection } from "@/components/settings/WorkspaceRulesSection";
import { SandboxSection } from "@/components/settings/SandboxSection";
import { AttachmentsSection } from "@/components/settings/AttachmentsSection";
import { ReviewSection } from "@/components/settings/ReviewSection";
import { PaneErrorBoundary } from "@/components/ErrorBoundary";
import { IconComb } from "@/components/flow/icon";

// ── Grouped nav (§6.5) ──────────────────────────────────────────────────────
// Settings is grouped by who a setting affects, not by a flat tab strip.
export type SettingsTab =
  | "account"
  | "appearance"
  | "folder-git"
  | "repositories"
  | "code-hosts"
  | "team-sync"
  | "guards"
  | "passkeys"
  | "rules"
  | "attachments"
  | "review"
  | "schedules"
  | "models"
  | "tools"
  | "permissions"
  | "sandbox"
  | "flow"
  | "updates"
  | "diagnostics"
  | "danger";

// `keywords` are extra search terms (synonyms + the controls a section contains)
// so search matches what a user actually types ("api key", "dark mode", "relay",
// "reset", "mcp") — not just the visible section label.
type NavItem = { id: SettingsTab; label: string; icon: React.ReactNode; keywords?: string[] };
type NavGroup = { cap: string; items: NavItem[] };

const NAV_GROUPS: NavGroup[] = [
  {
    cap: "You",
    items: [
      { id: "account", label: "Profile", icon: <IconUsers size={16} />, keywords: ["profile", "identity", "name", "avatar", "safety code", "fingerprint", "devices", "git email", "privacy", "typing", "presence"] },
      { id: "appearance", label: "Appearance", icon: <IconSparkle size={16} />, keywords: ["theme", "dark mode", "light", "color", "font"] },
    ],
  },
  {
    cap: "This workspace",
    items: [
      { id: "folder-git", label: "Folder & Git", icon: <IconFile size={16} />, keywords: ["project", "folder", "directory", "path", "git", "branch", "repo"] },
      { id: "repositories", label: "Repositories", icon: <IconFile size={16} />, keywords: ["repository", "repositories", "repo", "checkout", "clone", "coverage", "fork", "scan", "bind"] },
      { id: "code-hosts", label: "Code hosts", icon: <IconFile size={16} />, keywords: ["forge", "github", "gitlab", "codeberg", "forgejo", "gitea", "token", "clone", "repository", "pull request", "merge request"] },
      { id: "team-sync", label: "Team sync", icon: <IconUsers size={16} />, keywords: ["relay", "e2ee", "encryption", "sync", "token", "invite", "p2p", "peer", "collaborate"] },
      { id: "passkeys", label: "Passkeys & keys", icon: <IconLock size={16} />, keywords: ["passkey", "webauthn", "touch id", "recovery", "rules v2", "presence", "rotate", "workspace key", "keychain"] },
      { id: "rules", label: "Workspace rules", icon: <IconLock size={16} />, keywords: ["rules", "rules 4", "upgrade", "read-only", "readiness", "older devices", "pin", "model choice", "workspace"] },
      { id: "attachments", label: "File attachments", icon: <IconFile size={16} />, keywords: ["attachment", "file", "upload", "share", "quarantine", "blocklist", "hash", "malware", "scan", "policy"] },
      { id: "review", label: "Review", icon: <IconFile size={16} />, keywords: ["proposals", "archive", "auto-archive", "archived", "settled", "inbox", "dismiss"] },
      { id: "guards", label: "Guards", icon: <IconLock size={16} />, keywords: ["dearth", "security", "lockdown", "entrance", "approval", "throttle", "lease", "sandbox", "egress", "network", "preset", "registries", "npm", "connectors"] },
      { id: "schedules", label: "Schedules", icon: <IconActivity size={16} />, keywords: ["cron", "recurring", "timer", "automation"] },
    ],
  },
  {
    cap: "Agents",
    items: [
      { id: "models", label: "Models & runtimes", icon: <IconHexagon size={16} />, keywords: ["api key", "provider", "anthropic", "openai", "openrouter", "ollama", "claude", "model", "runtime", "endpoint", "agent", "prompt"] },
      { id: "tools", label: "Tools & MCP", icon: <IconWrench size={16} />, keywords: ["mcp", "server", "linear", "github", "tool", "integration"] },
      { id: "permissions", label: "Permissions", icon: <IconLock size={16} />, keywords: ["permission", "bypass", "shell", "edits", "file access", "safety"] },
      { id: "sandbox", label: "Sandbox", icon: <IconLock size={16} />, keywords: ["sandbox", "home", "folder", "read", "write", "grant", "allow", "path", "shell", "owner", "toolchain", "seatbelt", "landlock", "bubblewrap", "local server", "loopback", "agy"] },
      { id: "flow", label: "Flow", icon: <IconComb size={16} />, keywords: ["routing", "route", "shadow", "hand-off", "handoff", "queue", "review queue", "task board", "marks", "worktree"] },
    ],
  },
  {
    cap: "Advanced",
    items: [
      { id: "updates", label: "Updates & data", icon: <IconArrowDown size={16} />, keywords: ["update", "version", "upgrade", "data", "storage"] },
      { id: "diagnostics", label: "Diagnostics", icon: <IconActivity size={16} />, keywords: ["logs", "debug", "health", "troubleshoot"] },
      { id: "danger", label: "Danger zone", icon: <IconAlertTriangle size={16} />, keywords: ["reset", "delete", "wipe", "erase", "clear data", "factory"] },
    ],
  },
];

const TAB_LABEL: Record<SettingsTab, string> = Object.fromEntries(
  NAV_GROUPS.flatMap((g) => g.items.map((i) => [i.id, i.label])),
) as Record<SettingsTab, string>;

/// Flat {id,label} list of every settings tab, for the command palette.
export const SETTINGS_TABS: { id: SettingsTab; label: string }[] = NAV_GROUPS.flatMap((g) =>
  g.items.map((i) => ({ id: i.id, label: i.label })),
);

// Accepts a new id, a legacy tab name ("Account", "Team", …), or a deep-link
// fragment and resolves it to a current tab. Keeps the `settings:<tab>` tray
// route and older callers (e.g. FriendsView) working across the rename.
export function parseSettingsTab(raw: string | undefined | null): SettingsTab {
  switch ((raw ?? "").toLowerCase()) {
    case "account":
      return "account";
    case "appearance":
      return "appearance";
    case "folder-git":
    case "workspace":
      return "folder-git";
    case "repositories":
    case "repos":
      return "repositories";
    case "code-hosts":
    case "forges":
      return "code-hosts";
    case "team-sync":
    case "team":
      return "team-sync";
    case "passkeys":
    case "passkey":
      return "passkeys";
    case "rules":
    case "workspace-rules":
      return "rules";
    case "guards":
    case "security":
    case "dearth":
      return "guards";
    case "review":
    case "proposals":
    case "archive":
      return "review";
    case "attachments":
    case "files":
      return "attachments";
    case "schedules":
      return "schedules";
    case "models":
      return "models";
    case "tools":
      return "tools";
    case "permissions":
      return "permissions";
    case "sandbox":
    case "grants":
      return "sandbox";
    case "flow":
    case "routing":
      return "flow";
    case "updates":
      return "updates";
    case "diagnostics":
    case "logs":
      return "diagnostics";
    case "danger":
      return "danger";
    default:
      return "account";
  }
}

// The tab Settings last showed, so ⌘, reopens where you were.
let rememberedTab: SettingsTab = "account";
export const lastSettingsTab = () => rememberedTab;

/// Settings, presented as a focus-trapped sheet over the workspace (§6.5/§5.7).
/// It composes `Modal` from ui.tsx for the scrim + Escape/click-away + trap.
/// Nav is grouped and searchable; every section applies its changes immediately.
export function SettingsView({
  palette,
  onPaletteChange,
  appearanceMode,
  onAppearanceModeChange,
  initialTab,
  onClose,
  activeChat,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
  onAppearanceModeChange: (m: AppearanceMode) => void;
  /// Which tab to open on (defaults to Account). The tray uses this to jump
  /// straight to e.g. "Team sync".
  initialTab?: SettingsTab;
  onClose: () => void;
  /// The chat open behind Settings, for chat-level settings (network presets).
  activeChat?: { id: string; title: string } | null;
}) {
  const [tab, setTabState] = useState<SettingsTab>(initialTab ?? rememberedTab);
  const setTab = (t: SettingsTab) => {
    rememberedTab = t;
    setTabState(t);
  };
  rememberedTab = tab;
  const [query, setQuery] = useState("");

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return NAV_GROUPS;
    return NAV_GROUPS.map((g) => ({
      cap: g.cap,
      items: g.items.filter(
        (i) =>
          i.label.toLowerCase().includes(q) ||
          (i.keywords ?? []).some((k) => k.includes(q)),
      ),
    })).filter((g) => g.items.length > 0);
  }, [query]);

  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-center justify-center p-4"
      overlayStyle={{ background: "color-mix(in srgb, var(--ligus-canvas) 60%, transparent)" }}
      panelClassName="flex overflow-hidden"
      panelStyle={{
        width: 920,
        height: 680,
        maxWidth: "95vw",
        maxHeight: "90vh",
        borderRadius: "var(--ligus-radius-xl)",
        border: "1px solid var(--ligus-line)",
        boxShadow: "var(--ligus-shadow-pop)",
        background: "var(--ligus-canvas)",
        color: "var(--ligus-ink)",
      }}
    >
      {/* Nav column (§4: 208) */}
      <nav
        className="flex w-[208px] shrink-0 flex-col gap-3 overflow-y-auto border-r p-3"
        style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
      >
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search settings"
          aria-label="Search settings"
          className="w-full rounded-xl border px-3 py-1.5 text-[13px] outline-none focus:border-[color:var(--ligus-accent-cool)]"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
        />
        {filtered.map((g) => (
          <div key={g.cap} className="flex flex-col">
            <SectionCap>{g.cap}</SectionCap>
            {g.items.map((i) => (
              <NavRow
                key={i.id}
                icon={i.icon}
                label={i.label}
                active={tab === i.id}
                onClick={() => setTab(i.id)}
              />
            ))}
          </div>
        ))}
        {filtered.length === 0 && (
          <p className="px-2.5 text-xs text-ink-soft">No settings match “{query}”.</p>
        )}
      </nav>

      {/* Content column */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header
          className="flex shrink-0 items-center justify-between gap-3 border-b px-6 py-3"
          style={{ borderColor: "var(--ligus-line)" }}
        >
          <h1 className="ligus-title">{TAB_LABEL[tab]}</h1>
          <IconButton label="Close settings" onClick={onClose}>
            <IconX size={18} />
          </IconButton>
        </header>
        {/* Keyed by tab: each tab opens at its top instead of inheriting the
            previous tab's scroll offset. */}
        <div key={tab} className="ligus-enter min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {/* One boundary per section: a section that throws (an unexpected
              payload, a missing field) degrades to a contained card with
              "Try again" instead of replacing the whole app. */}
          <PaneErrorBoundary label={TAB_LABEL[tab]}>
          {tab === "account" && <AccountSection />}
          {tab === "appearance" && (
            <AppearanceSection
              palette={palette}
              onPaletteChange={onPaletteChange}
              appearanceMode={appearanceMode}
              onAppearanceModeChange={onAppearanceModeChange}
            />
          )}
          {tab === "folder-git" && <FolderGitSection />}
          {tab === "repositories" && <RepositoriesSection />}
          {tab === "code-hosts" && <CodeHostsSection />}
          {tab === "team-sync" && <TeamSyncSection />}
          {tab === "guards" && <GuardsSection chat={activeChat} />}
          {tab === "passkeys" && <SecuritySection />}
          {tab === "rules" && <WorkspaceRulesSection />}
          {tab === "attachments" && <AttachmentsSection />}
          {tab === "review" && <ReviewSection />}
          {tab === "schedules" && <SchedulesSection />}
          {tab === "models" && <ModelsSection />}
          {tab === "tools" && <ToolsSection />}
          {tab === "permissions" && <PermissionsSection />}
          {tab === "sandbox" && <SandboxSection />}
          {tab === "flow" && <FlowSection />}
          {tab === "updates" && <UpdatesSection />}
          {tab === "diagnostics" && <DiagnosticsSection />}
          {tab === "danger" && <DangerZoneSection />}
          </PaneErrorBoundary>
        </div>
      </div>
    </Modal>
  );
}
