import { Fragment, useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { listAgents, listChats, listWorkspaces } from "@/lib/ipc";
import { Modal } from "@/components/ui";
import { PANES, type UtilityPane } from "@/lib/panes";
import { SETTINGS_TABS } from "@/components/SettingsView";
import { fuzzyRank } from "@/lib/fuzzy";
import { loadRecents, noteRecent } from "@/lib/paletteRecents";
import { keyLabel, paneKey, shortcutKey } from "@/lib/shortcuts";
import { useChatAttention } from "@/state/useSessionStates";

export interface PaletteActions {
  newChat: () => void;
  openSettings: () => void;
  selectChat: (id: string) => void;
  selectWorkspace: (id: string) => void;
  /// Keyboard chat stepping (⌘⇧] / ⌘⇧[) and composer focus (⌘L).
  stepChat?: (dir: 1 | -1) => void;
  focusComposer?: () => void;
  /// Focus mode: collapse/restore the sidebar (⌘B) and tools rail (⌘J).
  toggleSidebar?: () => void;
  toggleTools?: () => void;
  /// Navigation + actions (all optional so the palette degrades gracefully).
  setCanvasMode?: (m: "chat" | "diff" | "code") => void;
  /// Jump to the code view and open the ⌘P quick-open modal.
  quickOpenFile?: () => void;
  /// Jump to the code view and reveal the integrated terminal.
  newTerminal?: () => void;
  openFriends?: () => void;
  openPane?: (pane: UtilityPane) => void;
  openSettingsTab?: (tab: string) => void;
  cycleAppearance?: () => void;
  /// Relaunch the spotlight feature tour.
  takeTour?: () => void;
  /// Admit a box (ligus serve / worker) to the active shared workspace.
  admitBox?: () => void;
  /// Open a chat's Review pane on one of its proposals.
  openProposal?: (sessionId: string, proposalId: string) => void;
  /// Start a message to an agent ("@name " in the composer).
  mentionAgent?: (name: string) => void;
  /// Settings → Profile with the add-a-phone panel open.
  addPhone?: () => void;
  /// The ⌘/ keyboard shortcuts sheet.
  showShortcuts?: () => void;
  /// Errors this session, including ones whose toast already faded.
  showErrors?: () => void;
}

interface Cmd {
  id: string;
  label: string;
  hint?: string;
  /// Extra words that match but aren't shown.
  keywords?: string;
  /// Waiting on you: listed first when nothing is typed.
  needsYou?: boolean;
  run: () => void;
}

/// How much a recent item is lifted when ranking typed queries.
function recentBonus(index: number): number {
  return index < 0 ? 0 : 40 - index * 2;
}

/// ⌘K / Ctrl-K command palette: fuzzy-jump to a chat, switch workspace, or run
/// a quick action. Data is only fetched while open; no extra deps.
export function CommandPalette({
  open,
  onClose,
  actions,
  sessionId = null,
  workspaceId = "",
}: {
  open: boolean;
  onClose: () => void;
  actions: PaletteActions;
  /// The open chat (its agents are offered as items).
  sessionId?: string | null;
  /// The active workspace (its chats' open proposals are offered as items).
  workspaceId?: string;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [recents, setRecents] = useState<string[]>([]);

  const chats = useQuery({ queryKey: ["chats"], queryFn: listChats, enabled: open });
  const workspaces = useQuery({ queryKey: ["workspaces-palette"], queryFn: listWorkspaces, enabled: open });
  const attention = useChatAttention(workspaceId, open && Boolean(actions.openProposal));
  const agents = useQuery({
    queryKey: ["agents", sessionId],
    queryFn: () => listAgents(sessionId ?? ""),
    enabled: open && Boolean(sessionId) && Boolean(actions.mentionAgent),
  });

  // Reset on open; Modal autofocuses the input (its first control).
  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
      setRecents(loadRecents());
    }
  }, [open]);

  const commands = useMemo<Cmd[]>(() => {
    const items: Cmd[] = [];
    const run = (fn: () => void) => () => {
      fn();
      onClose();
    };
    // Proposals that wait on a decision, across the workspace's chats; the
    // ones waiting on you lead the list when nothing is typed.
    if (actions.openProposal) {
      const titles = new Map((chats.data ?? []).map((c) => [c.id, c.title || "Untitled chat"]));
      for (const a of attention.data ?? []) {
        for (const p of a.proposals) {
          items.push({
            id: `proposal-${p.id}`,
            label: `Review “${p.title || "Untitled proposal"}”`,
            hint: p.status === "approved" ? "Proposal · ready to apply" : p.needsMe ? "Proposal · needs you" : "Proposal",
            keywords: `proposal review approve ${titles.get(a.sessionId) ?? ""}`,
            needsYou: p.needsMe,
            run: run(() => actions.openProposal!(a.sessionId, p.id)),
          });
        }
      }
    }
    items.push(
      { id: "new-chat", label: "New chat", hint: "Action", run: run(actions.newChat) },
      { id: "settings", label: "Open settings", hint: `Action · ${keyLabel(shortcutKey("settings")!)}`, run: run(actions.openSettings) },
    );
    if (actions.setCanvasMode) {
      items.push(
        { id: "view-chat", label: "View: Chat", hint: "Go", run: run(() => actions.setCanvasMode!("chat")) },
        { id: "view-diff", label: "View: Diff (uncommitted changes)", hint: "Go", run: run(() => actions.setCanvasMode!("diff")) },
        { id: "view-code", label: "View: Code", hint: "Go", run: run(() => actions.setCanvasMode!("code")) },
      );
      if (actions.quickOpenFile) {
        items.push({ id: "quick-open", label: "Quick open file", hint: `Code · ${keyLabel(shortcutKey("quickOpen")!)}`, run: run(actions.quickOpenFile) });
      }
      if (actions.newTerminal) {
        items.push({ id: "new-terminal", label: "New terminal", hint: "Code", run: run(actions.newTerminal) });
      }
    }
    if (actions.openFriends) {
      items.push({ id: "friends", label: "Open People & DMs", hint: "Go", run: run(actions.openFriends) });
    }
    if (actions.openPane) {
      for (const p of PANES) {
        const key = paneKey(p.id);
        items.push({
          id: `pane-${p.id}`,
          label: `Open ${p.label}`,
          hint: key ? `Pane · ${keyLabel(key)}` : "Pane",
          run: run(() => actions.openPane!(p.id)),
        });
      }
    }
    if (actions.stepChat) {
      items.push(
        { id: "next-chat", label: "Next chat", hint: `Go · ${keyLabel(shortcutKey("nextChat")!)}`, run: run(() => actions.stepChat!(1)) },
        { id: "prev-chat", label: "Previous chat", hint: `Go · ${keyLabel(shortcutKey("prevChat")!)}`, run: run(() => actions.stepChat!(-1)) },
      );
    }
    if (actions.focusComposer) {
      items.push({ id: "focus-composer", label: "Focus the message box", hint: `Go · ${keyLabel(shortcutKey("composer")!)}`, run: run(actions.focusComposer) });
    }
    if (actions.cycleAppearance) {
      items.push({ id: "appearance", label: "Toggle light / dark", hint: "Action", run: run(actions.cycleAppearance) });
    }
    if (actions.addPhone) {
      items.push({
        id: "add-phone",
        label: "Add a phone…",
        hint: "Devices",
        keywords: "iphone mobile pair qr device enroll",
        run: run(actions.addPhone),
      });
    }
    if (actions.showShortcuts) {
      items.push({
        id: "shortcuts",
        label: "Keyboard shortcuts",
        hint: `Help · ${keyLabel(shortcutKey("shortcuts")!)}`,
        keywords: "keys hotkeys keybindings",
        run: run(actions.showShortcuts),
      });
    }
    if (actions.showErrors) {
      items.push({
        id: "errors",
        label: "Recent errors",
        hint: "Help",
        keywords: "log failures problems toast",
        run: run(actions.showErrors),
      });
    }
    if (actions.admitBox) {
      items.push({ id: "admit-box", label: "Admit a box…", hint: "Workspace", run: run(actions.admitBox) });
    }
    if (actions.takeTour) {
      items.push({ id: "take-tour", label: "Take the tour", hint: "Help", run: run(actions.takeTour) });
    }
    if (actions.openSettingsTab) {
      for (const t of SETTINGS_TABS) {
        items.push({
          id: `settings-${t.id}`,
          label: `Settings: ${t.label}`,
          hint: "Settings",
          run: run(() => actions.openSettingsTab!(t.id)),
        });
      }
    }
    if (actions.toggleSidebar) {
      items.push({
        id: "toggle-sidebar",
        label: "Toggle sidebar",
        hint: `View · ${keyLabel(shortcutKey("toggleSidebar")!)}`,
        run: run(actions.toggleSidebar),
      });
    }
    if (actions.toggleTools) {
      items.push({
        id: "toggle-tools",
        label: "Toggle tools rail",
        hint: `View · ${keyLabel(shortcutKey("toggleTools")!)}`,
        run: run(actions.toggleTools),
      });
    }
    for (const w of workspaces.data ?? []) {
      items.push({
        id: `ws-${w.id}`,
        label: `Switch to ${w.name}`,
        hint: w.kind === "room" ? "Workspace · room" : "Workspace",
        run: run(() => actions.selectWorkspace(w.id)),
      });
    }
    for (const c of (chats.data ?? []).filter((c) => !c.archived)) {
      items.push({
        id: `chat-${c.id}`,
        label: c.title || "Untitled chat",
        hint: "Chat",
        run: run(() => actions.selectChat(c.id)),
      });
    }
    if (actions.mentionAgent) {
      for (const a of agents.data ?? []) {
        items.push({
          id: `agent-${a.id}`,
          label: `@${a.name}`,
          hint: "Agent · message",
          keywords: `agent mention ${a.role}`,
          run: run(() => actions.mentionAgent!(a.name)),
        });
      }
    }
    // Remember what was run, so it comes back first next time.
    return items.map((c) => ({
      ...c,
      run: () => {
        setRecents(noteRecent(c.id));
        c.run();
      },
    }));
  }, [chats.data, workspaces.data, attention.data, agents.data, actions, onClose]);

  // Nothing typed: recent items, then what waits on you, then the rest in
  // their usual order. Typed: fuzzy ranking, with recent items lifted.
  const { filtered, recentCount, needsYouCount } = useMemo(() => {
    const q = query.trim();
    const recentIndex = new Map(recents.map((id, i) => [id, i]));
    if (!q) {
      const recent = recents.map((id) => commands.find((c) => c.id === id)).filter((c): c is Cmd => Boolean(c));
      const seen = new Set(recent.map((c) => c.id));
      const waiting = commands.filter((c) => c.needsYou && !seen.has(c.id));
      for (const c of waiting) seen.add(c.id);
      const rest = commands.filter((c) => !seen.has(c.id));
      return { filtered: [...recent, ...waiting, ...rest], recentCount: recent.length, needsYouCount: waiting.length };
    }
    const ranked = fuzzyRank(
      commands,
      q,
      (c) => `${c.label} ${c.hint ?? ""} ${c.keywords ?? ""}`,
      (c) => recentBonus(recentIndex.get(c.id) ?? -1) + (c.needsYou ? 10 : 0),
    );
    return { filtered: ranked, recentCount: 0, needsYouCount: 0 };
  }, [commands, query, recents]);

  // Keep the highlighted item in view while arrowing through a long list.
  const activeId = filtered[active]?.id;
  useEffect(() => {
    if (!activeId) return;
    document.querySelector<HTMLElement>(`[data-palette-item="${CSS.escape(activeId)}"]`)?.scrollIntoView?.({ block: "nearest" });
  }, [activeId]);

  // Keep the active index in range as the filtered list shrinks.
  useEffect(() => {
    setActive((a) => Math.min(a, Math.max(0, filtered.length - 1)));
  }, [filtered.length]);

  if (!open) return null;

  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-start justify-center pt-[12vh]"
      overlayStyle={{ background: "color-mix(in srgb, var(--ligus-ink) 45%, transparent)" }}
      panelClassName="w-full max-w-lg overflow-hidden rounded-2xl border shadow-2xl"
      panelStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
    >
      <input
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="Search chats, proposals, agents, actions…"
        className="w-full border-b bg-transparent px-4 py-3 text-base outline-none"
        style={{ borderColor: "var(--ligus-line)" }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setActive((a) => Math.min(a + 1, filtered.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            filtered[active]?.run();
          }
        }}
      />
      <div role="listbox" aria-label="Results" className="max-h-[50vh] overflow-y-auto py-1">
        {filtered.length === 0 && <div className="px-4 py-6 text-center text-sm text-ink-soft">No matches.</div>}
        {filtered.map((c, i) => (
          <Fragment key={c.id}>
            {i === 0 && recentCount > 0 && <PaletteCaption text="Recent" />}
            {i === recentCount && needsYouCount > 0 && <PaletteCaption text="Needs you" />}
            {i === recentCount + needsYouCount && (recentCount > 0 || needsYouCount > 0) && <PaletteCaption text="Everything" />}
            <button
              role="option"
              aria-selected={i === active}
              data-palette-item={c.id}
              onMouseEnter={() => setActive(i)}
              onClick={c.run}
              className="flex w-full items-center justify-between gap-3 px-4 py-2.5 text-left text-sm"
              style={{ background: i === active ? "var(--ligus-mist)" : "transparent" }}
            >
              <span className="truncate">{c.label}</span>
              {c.hint && <span className="shrink-0 text-xs text-ink-soft">{c.hint}</span>}
            </button>
          </Fragment>
        ))}
      </div>
      <div
        className="flex items-center gap-3 border-t px-4 py-2 text-[11px] text-ink-soft"
        style={{ borderColor: "var(--ligus-line)" }}
      >
        <span>↑↓ navigate</span>
        <span>↵ select</span>
        <span>esc close</span>
        <span className="ml-auto">{keyLabel(shortcutKey("shortcuts")!)} shortcuts</span>
      </div>
    </Modal>
  );
}

function PaletteCaption({ text }: { text: string }) {
  return (
    <div className="px-4 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wide text-ink-soft" aria-hidden>
      {text}
    </div>
  );
}
