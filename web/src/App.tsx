import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addWorkspaceToList,
  createChat,
  ensureSelfMember,
  getAppSettings,
  markOnboarded,
  markTourSeen,
  listWorkspaces,
  getChat,
  listChats,
  getContextTelemetry,
  listRuntimes,
  maybeRespond,
  notifyMentions,
  onChatStream,
  onChatActivity,
  pickWorkspaceFolder,
  onTrayNavigate,
  onWorkspaceSynced,
  onSyncError,
  onWorkspaceRemoved,
  removeWorkspaceFromList,
  renameChat,
  saveAppearance,
  setActiveWorkspace,
  setActiveChat,
  setWorkspaceRoot,
  getChatLockdown,
  getDearthState,
} from "@/lib/ipc";
import { setShownWorkspace } from "@/lib/shownWorkspace";
import { Sidebar } from "@/components/Sidebar";
import { ChatView, runtimePickerLabel } from "@/components/ChatView";
import { ChatRunsOn } from "@/components/ChatRepoChip";
import { editorStore } from "@/state/editorStore";
import { liveTurns } from "@/state/liveTurns";
// Lazy so Monaco (the bulk of the bundle) only loads when the Diff view opens.
const DiffView = lazy(() => import("@/components/DiffView").then((m) => ({ default: m.DiffView })));
// Lazy so Monaco + xterm (the code view's heavy deps) only load when it opens.
const CodeView = lazy(() => import("@/components/editor/CodeView").then((m) => ({ default: m.CodeView })));
import type { CodeViewIntent } from "@/components/editor/CodeView";
import { SettingsView, lastSettingsTab, parseSettingsTab, type SettingsTab } from "@/components/SettingsView";
import { RightRail } from "@/components/RightRail";
import { Onboarding } from "@/components/Onboarding";
import { Tour, type TourStep } from "@/components/Tour";
import { ToastHost, toast, errMsg } from "@/components/Toast";
import { PresenceSheet } from "@/components/PresenceSheet";
import { PhoneEnrollTripwire } from "@/components/PhoneEnrollTripwire";
import { DearthBanner } from "@/components/guards/DearthBanner";
import { LockdownBar } from "@/components/guards/LockdownBar";
import { AppStatusBar } from "@/components/AppStatusBar";
import { blockingBanner } from "@/lib/banners";
import { DialogHost, promptDialog } from "@/components/Dialog";
import { IconPencil, IconHexagon, IconStateNeedsYou } from "@/lib/icons";
import { CanvasLoading, EmptyCanvas } from "@/components/EmptyCanvas";
import { CommandPalette } from "@/components/CommandPalette";
import { ShortcutsSheet } from "@/components/ShortcutsSheet";
import { ErrorLogSheet } from "@/components/ErrorLogSheet";
import { Inbox } from "@/components/Inbox";
import { useDockBadge, useNeedsYou } from "@/state/useNeedsYou";
import { needsYouCount, needsYouLabel } from "@/lib/needsYou";
import { keyLabel, matchShortcut, shortcutKey } from "@/lib/shortcuts";
import { AdmitBoxModal } from "@/components/AdmitBoxModal";
import { WorkspaceRail } from "@/components/WorkspaceRail";
import { FriendsView } from "@/components/FriendsView";
import { AddWorkspaceModal } from "@/components/AddWorkspaceModal";
import { BindWorkspacePrompt } from "@/components/BindWorkspacePrompt";
import { PaneErrorBoundary } from "@/components/ErrorBoundary";
import { CANVAS_TAB_HINT, TOUR_CANVAS_TABS, TOUR_DIFF } from "@/lib/tour";
import { needsOnboarding as needsFirstRunOnboarding, shouldAutoTour } from "@/lib/firstRun";
import { ADMIT_BOX_EVENT, OPEN_CHAT_EVENT, OPEN_SETTINGS_EVENT, flashMessage, requestAddPhone, type OpenChatDetail } from "@/lib/nav";
import {
  applyTheme,
  loadMode,
  loadTheme,
  resolvePalette,
  saveMode,
  savePalette,
  watchSystemScheme,
  type AppearanceMode,
  type ThemeName,
} from "@/lib/theme";

type View = "workspace" | "friends" | "inbox";
type CanvasMode = "chat" | "diff" | "code";
type UtilityPane =
  | "tools"
  | "review"
  | "people"
  | "vaults"
  | "skills"
  | "workflows"
  | "flow"
  | "memory"
  | "activity"
  | "context";

const UI_SCALE_STORAGE_KEY = "ligus.uiScale";
const DEFAULT_UI_SCALE = 1;
const MIN_UI_SCALE = 0.85;
const MAX_UI_SCALE = 1.2;
const MENU_PANE_WIDTH_STORAGE_KEY = "ligus.menuPaneWidth";
const UTILITY_PANE_WIDTH_STORAGE_KEY = "ligus.utilityPaneWidth";
const DEFAULT_MENU_PANE_WIDTH = 260;
const DEFAULT_UTILITY_PANE_WIDTH = 340;
const MIN_MENU_PANE_WIDTH = 200;
const MAX_MENU_PANE_WIDTH = 420;
const MIN_UTILITY_PANE_WIDTH = 220;
const MAX_UTILITY_PANE_WIDTH = 520;
const MIN_CHAT_WIDTH = 420;

function clampUiScale(value: number) {
  return Math.min(MAX_UI_SCALE, Math.max(MIN_UI_SCALE, value));
}

function clampPaneWidth(value: number, min: number, max: number) {
  if (max <= min) return min;
  return Math.min(max, Math.max(min, value));
}

function loadUiScale() {
  if (typeof window === "undefined") return DEFAULT_UI_SCALE;
  const raw = window.localStorage.getItem(UI_SCALE_STORAGE_KEY);
  // A missing key must mean 100%, not Number(null) = 0 clamped to 85%.
  const parsed = raw == null || raw.trim() === "" ? NaN : Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? clampUiScale(parsed) : DEFAULT_UI_SCALE;
}

function loadPaneWidth(storageKey: string, fallback: number, min: number, max: number) {
  if (typeof window === "undefined") return fallback;
  const raw = window.localStorage.getItem(storageKey);
  // Number(null) is 0, which used to clamp a fresh install to the minimum
  // width instead of the default.
  const parsed = raw == null || raw.trim() === "" ? NaN : Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? clampPaneWidth(parsed, min, max) : fallback;
}

export function App() {
  const [palette, setPaletteState] = useState<ThemeName>(loadTheme());
  const [appearanceMode, setAppearanceModeState] = useState<AppearanceMode>(loadMode());
  const [view, setView] = useState<View>("workspace");
  // Settings is a presented sheet over the workspace, not a view — opening it
  // never unmounts the chat behind it. A nonce remounts the sheet so a repeat
  // open (including repeat tray clicks) re-applies the requested tab.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("account");
  const [settingsNonce, setSettingsNonce] = useState(0);

  // Open the Settings sheet on a given tab. Accepts new ids, legacy names, or a
  // deep-link fragment (parseSettingsTab normalizes them).
  function openSettings(tab?: string) {
    setSettingsTab(tab ? parseSettingsTab(tab) : lastSettingsTab());
    setSettingsNonce((n) => n + 1);
    setSettingsOpen(true);
  }
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [errorsOpen, setErrorsOpen] = useState(false);
  // Focus mode: only the chat and the message box. It hides the workspace
  // rail, sidebar, tools rail and status bar without forgetting whether they
  // were open, so leaving it puts everything back.
  const [focusMode, setFocusMode] = useState(false);
  const focusRef = useRef(false);
  focusRef.current = focusMode;
  const [addWsOpen, setAddWsOpen] = useState(false);
  // "Admit a box" for the active workspace, from the workspace menu or ⌘K
  // (the rail's right-click menu keeps its own entry).
  const [admitBoxOpen, setAdmitBoxOpen] = useState(false);
  const [mode, setMode] = useState<CanvasMode>("chat");
  // Live mirror of `mode` for non-React callbacks (the editorStore subscription
  // below) so they read the current canvas mode without re-subscribing.
  const modeRef = useRef(mode);
  modeRef.current = mode;
  // One-shot request handed to the code view (⌘P / New terminal from the
  // palette); CodeView consumes it and calls back to clear it.
  const [codePending, setCodePending] = useState<CodeViewIntent | null>(null);
  // Wedge plumbing (P2): the editor and diff/chat views can't reach each other
  // directly (CodeView sits between App and EditorView), so cross-view requests
  // ride window events into App and are handed back down as one-shot props.
  // A file the editor asked the Diff view to select ("Open in Diff").
  const [pendingDiffPath, setPendingDiffPath] = useState<string | null>(null);
  // A snippet the editor asked to drop into the chat composer ("Send selection").
  const [composerSeed, setComposerSeed] = useState<string | null>(null);
  const [utilityPane, setUtilityPane] = useState<UtilityPane>("tools");
  // Pane visibility persists — a focused layout should survive restarts.
  const [showUtilityPane, setShowUtilityPane] = useState(
    () => window.localStorage.getItem("ligus.utilityPaneVisible") !== "0",
  );
  const [sidebarVisible, setSidebarVisible] = useState(
    () => window.localStorage.getItem("ligus.sidebarVisible") !== "0",
  );
  useEffect(() => {
    window.localStorage.setItem("ligus.utilityPaneVisible", showUtilityPane ? "1" : "0");
  }, [showUtilityPane]);
  useEffect(() => {
    window.localStorage.setItem("ligus.sidebarVisible", sidebarVisible ? "1" : "0");
  }, [sidebarVisible]);
  // Suppress the webview's default right-click menu (Reload/Inspect in dev; a bare
  // menu in prod) on app chrome, so our own themed context menus are the only ones
  // that appear. Text inputs / contenteditable keep the native menu (copy/paste),
  // and any element that opens its own menu calls preventDefault first (this runs
  // last, so it never double-fires).
  useEffect(() => {
    const onCtx = (e: MouseEvent) => {
      if (e.defaultPrevented) return; // a component already handled it
      const t = e.target as HTMLElement | null;
      if (t?.closest("input, textarea, [contenteditable='true'], [contenteditable='']")) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", onCtx);
    return () => document.removeEventListener("contextmenu", onCtx);
  }, []);
  const [uiScale, setUiScale] = useState(loadUiScale);
  const [menuPaneWidth, setMenuPaneWidth] = useState(() =>
    loadPaneWidth(
      MENU_PANE_WIDTH_STORAGE_KEY,
      DEFAULT_MENU_PANE_WIDTH,
      MIN_MENU_PANE_WIDTH,
      MAX_MENU_PANE_WIDTH,
    ),
  );
  const [utilityPaneWidth, setUtilityPaneWidth] = useState(() =>
    loadPaneWidth(
      UTILITY_PANE_WIDTH_STORAGE_KEY,
      DEFAULT_UTILITY_PANE_WIDTH,
      MIN_UTILITY_PANE_WIDTH,
      MAX_UTILITY_PANE_WIDTH,
    ),
  );
  const previousOverflowCount = useRef(0);
  // Whether the Context pane was already auto-surfaced for the current chat, so we
  // do it at most once and don't repeatedly hijack the user's layout.
  const autoOpenedContext = useRef(false);
  const qc = useQueryClient();

  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  // The active workspace's name (the team/room identity) — the sidebar header
  // labels the space by name, not by its project-folder basename.
  const workspaceList = useQuery({ queryKey: ["workspaces"], queryFn: listWorkspaces });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  // First-run gate: block the shell with onboarding until the backend says
  // THIS data dir finished it (settings.json). The webview's localStorage
  // outlives a deleted or reset data dir, so it is never consulted here.
  const needsOnboarding = needsFirstRunOnboarding(settings.data);

  // Feature tour: a spotlight walk through the shell that DRIVES the UI (it flips
  // the canvas mode and reveals panes as it goes). It's non-destructive — the
  // layout it rearranges is snapshotted on start and restored on close, so the
  // tour never leaves the app in a different state than it found it.
  const [tourOpen, setTourOpen] = useState(false);
  const tourSnapshot = useRef<{ mode: CanvasMode; utility: boolean; view: View } | null>(null);
  function startTour() {
    tourSnapshot.current = { mode, utility: showUtilityPane, view };
    setSettingsOpen(false);
    setView("workspace");
    setTourOpen(true);
  }
  function endTour() {
    setTourOpen(false);
    if (settings.data && !settings.data.tourSeen) {
      void markTourSeen()
        .then(() => qc.invalidateQueries({ queryKey: ["settings"] }))
        .catch(() => {
          /* best effort: the tour may auto-run once more next launch */
        });
    }
    const snap = tourSnapshot.current;
    if (snap) {
      setMode(snap.mode);
      setShowUtilityPane(snap.utility);
      setView(snap.view);
    }
    tourSnapshot.current = null;
  }
  // Auto-run once per data dir, right after setup.
  const autoTour = shouldAutoTour(settings.data);
  useEffect(() => {
    if (!autoTour || tourOpen) return;
    const t = window.setTimeout(() => startTour(), 450);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoTour]);
  const activeChat = useQuery({
    queryKey: ["chat", selectedId],
    queryFn: () => getChat(selectedId ?? ""),
    enabled: Boolean(selectedId),
  });
  const contextTelemetry = useQuery({
    queryKey: ["context-telemetry", selectedId, activeChat.data?.runtimeId],
    queryFn: () => getContextTelemetry(selectedId ?? ""),
    enabled: Boolean(selectedId),
  });
  // Apply the resolved palette whenever the accent or the light/dark mode
  // changes, and re-resolve live when the OS appearance flips (mode = auto).
  useEffect(() => {
    applyTheme(resolvePalette(palette, appearanceMode));
    // Best-effort copy for `ligus serve` (a phone set to "Match host").
    void saveAppearance(palette, appearanceMode).catch(() => {});
    return watchSystemScheme(() => applyTheme(resolvePalette(palette, appearanceMode)));
  }, [appearanceMode, palette]);


  const setPalette = (next: ThemeName) => {
    savePalette(next);
    setPaletteState(next);
  };
  const setAppearanceMode = (next: AppearanceMode) => {
    saveMode(next);
    setAppearanceModeState(next);
  };

  useEffect(() => {
    document.documentElement.style.fontSize = `${uiScale * 100}%`;
    window.localStorage.setItem(UI_SCALE_STORAGE_KEY, String(uiScale));
  }, [uiScale]);

  useEffect(() => {
    window.localStorage.setItem(MENU_PANE_WIDTH_STORAGE_KEY, String(menuPaneWidth));
  }, [menuPaneWidth]);

  useEffect(() => {
    window.localStorage.setItem(UTILITY_PANE_WIDTH_STORAGE_KEY, String(utilityPaneWidth));
  }, [utilityPaneWidth]);

  // Live callbacks for the global key handler (registered once).
  const stepChatRef = useRef<(dir: 1 | -1) => void>(() => {});
  const togglePaneRef = useRef<(pane: UtilityPane) => void>(() => {});

  useEffect(() => {
    // The bindings live in lib/shortcuts (the ⌘/ sheet reads the same list).
    const handleKeyDown = (event: KeyboardEvent) => {
      const hit = matchShortcut(event);
      if (!hit) return;
      switch (hit.id) {
        case "nextChat":
        case "prevChat":
          event.preventDefault();
          stepChatRef.current(hit.id === "nextChat" ? 1 : -1);
          return;
        case "pane":
          event.preventDefault();
          togglePaneRef.current(hit.pane as UtilityPane);
          return;
        case "composer": {
          // From anywhere: rail, sidebar, a dialog-free canvas.
          const composer = document.querySelector<HTMLTextAreaElement>("textarea[data-composer]");
          if (composer) {
            event.preventDefault();
            composer.focus();
          }
          return;
        }
        case "palette":
          event.preventDefault();
          setShortcutsOpen(false);
          setPaletteOpen((v) => !v);
          return;
        case "shortcuts":
          event.preventDefault();
          setPaletteOpen(false);
          setShortcutsOpen((v) => !v);
          return;
        // VS Code's muscle memory. Both persist. In focus mode either key
        // leaves it and shows that panel.
        case "toggleSidebar":
          event.preventDefault();
          if (focusRef.current) {
            setFocusMode(false);
            setSidebarVisible(true);
            return;
          }
          setSidebarVisible((v) => !v);
          return;
        case "toggleTools":
          event.preventDefault();
          if (focusRef.current) {
            setFocusMode(false);
            setShowUtilityPane(true);
            return;
          }
          setShowUtilityPane((v) => !v);
          return;
        case "focusMode":
          event.preventDefault();
          setFocusMode((v) => !v);
          return;
        case "settings":
          event.preventDefault();
          openSettings();
          return;
        case "zoomReset":
          event.preventDefault();
          setUiScale(DEFAULT_UI_SCALE);
          return;
        case "zoomIn":
          event.preventDefault();
          setUiScale((value) => clampUiScale(Number((value + 0.05).toFixed(2))));
          return;
        case "zoomOut":
          event.preventDefault();
          setUiScale((value) => clampUiScale(Number((value - 0.05).toFixed(2))));
          return;
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Wedge: when any view asks the editor to open a file (editorStore.requestOpen
  // from the Diff view, an @file chip in chat, a proposal, …), reveal the code
  // canvas so the request isn't lost behind another mode. We only READ
  // pendingOpen to decide whether to switch — CodeView still owns consuming and
  // clearing it, so we never double-clear. Subscribing imperatively (rather than
  // via useEditorStore) keeps App from re-rendering on every editor keystroke.
  useEffect(() => {
    const unsub = editorStore.subscribe(() => {
      if (editorStore.getState().pendingOpen && modeRef.current !== "code") {
        setView("workspace");
        setMode("code");
      }
    });
    return () => {
      unsub();
    };
  }, []);

  // Wedge: the editor is nested under CodeView (which App can't pass props to),
  // so its "Open in Diff" / "Send selection to chat" controls emit window events
  // App turns into a mode switch plus a one-shot prop for the target view.
  useEffect(() => {
    const onOpenInDiff = (e: Event) => {
      const path = (e as CustomEvent<{ path?: string }>).detail?.path;
      if (!path) return;
      setView("workspace");
      setMode("diff");
      setPendingDiffPath(path);
    };
    const onSendToChat = (e: Event) => {
      const text = (e as CustomEvent<{ text?: string }>).detail?.text;
      if (!text) return;
      setView("workspace");
      setMode("chat");
      setComposerSeed(text);
    };
    window.addEventListener("ligus:editor-open-in-diff", onOpenInDiff);
    window.addEventListener("ligus:editor-send-to-chat", onSendToChat);
    return () => {
      window.removeEventListener("ligus:editor-open-in-diff", onOpenInDiff);
      window.removeEventListener("ligus:editor-send-to-chat", onSendToChat);
    };
  }, []);

  useEffect(() => {
    const unlisten = onWorkspaceSynced(() => {
      // A sync succeeded: refresh the health pill (flips back to Live on recovery).
      qc.invalidateQueries({ queryKey: ["sync-status"] });
      qc.invalidateQueries({ queryKey: ["dearth-state"] });
      qc.invalidateQueries({ queryKey: ["chat-lockdown"] });
      qc.invalidateQueries({ queryKey: ["chats"] });
      qc.invalidateQueries({ queryKey: ["chat"] });
      // Channels + skills also ride the synced log (config hoist / §11): refresh
      // them too, or a channel/skill a headless agent created while you were
      // offline lands in the store but not in the sidebar until a reload.
      qc.invalidateQueries({ queryKey: ["channels"] });
      qc.invalidateQueries({ queryKey: ["skills"] });
      qc.invalidateQueries({ queryKey: ["context-telemetry"] });
      qc.invalidateQueries({ queryKey: ["proposals"] });
      qc.invalidateQueries({ queryKey: ["members"] });
      qc.invalidateQueries({ queryKey: ["agents"] });
      qc.invalidateQueries({ queryKey: ["vaults"] });
      // Queued work (unanswered mentions + host status) rides the log too, so a
      // mention/host-heartbeat that synced while offline updates the queue live.
      qc.invalidateQueries({ queryKey: ["queued-work"] });
      // Mention highlights: a synced message may @-mention the local user, so
      // refresh which channels/chats light up in the sidebar — and which
      // background workspaces flag an unread mention in the switcher.
      qc.invalidateQueries({ queryKey: ["mention-states"] });
      qc.invalidateQueries({ queryKey: ["all-mention-states"] });
      // Cross-device dispatch: if a teammate's message just synced into the open
      // chat and we own the responder, answer it (no-op otherwise).
      if (selectedId) {
        void maybeRespond(selectedId).catch(() => {});
        // Ping the local user if a synced message @-mentions them.
        void notifyMentions(selectedId).catch(() => {});
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, selectedId]);

  // A sync failed: refresh the health pill so it flips to "Sync error" the
  // moment the relay/key/token goes bad (mirrors onWorkspaceSynced above).
  useEffect(() => {
    const unlisten = onSyncError(() => {
      qc.invalidateQueries({ queryKey: ["sync-status"] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc]);

  // Removed from the active workspace by an owner/admin: tell the user (removals
  // were previously silent), and refresh the roster/chats so their view reflects
  // the loss of access.
  useEffect(() => {
    const unlisten = onWorkspaceRemoved(() => {
      toast.error("You've been removed from this workspace.");
      qc.invalidateQueries({ queryKey: ["members"] });
      qc.invalidateQueries({ queryKey: ["chats"] });
      qc.invalidateQueries({ queryKey: ["sync-status"] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc]);

  // In-app links to a Settings tab (e.g. a refused invite's "Open Team
  // sync"): close the modal they came from and open the sheet.
  useEffect(() => {
    const onOpen = (e: Event) => {
      setAddWsOpen(false);
      openSettings((e as CustomEvent<string>).detail);
    };
    window.addEventListener(OPEN_SETTINGS_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_SETTINGS_EVENT, onOpen);
    // openSettings is stable (only touches setState setters).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // In-app links to "Admit a box" (the add-a-phone check). The dialog renders
  // above Settings, so the person comes back to where they were.
  useEffect(() => {
    const onAdmit = () => setAdmitBoxOpen(true);
    window.addEventListener(ADMIT_BOX_EVENT, onAdmit);
    return () => window.removeEventListener(ADMIT_BOX_EVENT, onAdmit);
  }, []);

  // System-tray navigation: the native menu emits a route string and we switch
  // the in-window view to match (Settings/Friends are views, not OS windows).
  useEffect(() => {
    const unlisten = onTrayNavigate((route) => {
      if (route === "friends") {
        setView("friends");
      } else if (route === "workspace") {
        setView("workspace");
      } else if (route.startsWith("settings")) {
        const tab = route.includes(":") ? route.split(":")[1] : "account";
        openSettings(tab);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
    // openSettings is stable (only touches setState setters).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Register the local user in the chat's roster on open, so People populates.
  useEffect(() => {
    if (!selectedId) return;
    ensureSelfMember(selectedId)
      .then(() => qc.invalidateQueries({ queryKey: ["members", selectedId] }))
      .catch(() => {});
  }, [qc, selectedId]);

  // Point the backend's effective code dir at the active chat's isolated worktree
  // so the editor/terminal/diff operate on that chat's tree; refetch settings +
  // diffs so workspaceRoot-derived views follow. No-op fallback on a non-git or
  // unbound workspace (the backend reverts to the workspace folder).
  useEffect(() => {
    if (view !== "workspace") return;
    void setActiveChat(selectedId)
      .then(() => {
        qc.invalidateQueries({ queryKey: ["settings"] });
        qc.invalidateQueries({ queryKey: ["diffs"] });
      })
      .catch(() => {});
  }, [qc, selectedId, view]);

  // Live turn state for every chat (not just the open one), for the
  // sidebar's state chips. One subscription for the app's lifetime.
  useEffect(() => {
    const unStream = onChatStream((e) => liveTurns.onStream(e));
    const unActivity = onChatActivity((e) => liveTurns.onActivity(e.sessionId));
    // A turn that went silent stops reading as working without an event.
    const tick = window.setInterval(() => liveTurns.refresh(), 30_000);
    return () => {
      window.clearInterval(tick);
      unStream.then((fn) => fn());
      unActivity.then((fn) => fn());
    };
  }, []);
  // The chat on screen: a turn you watch finish isn't news.
  useEffect(() => {
    liveTurns.setViewing(view === "workspace" && mode === "chat" ? selectedId : null);
  }, [selectedId, view, mode]);

  useEffect(() => {
    const unlisten = onChatStream((event) => {
      if (event.sessionId !== selectedId) return;
      // Only refresh context telemetry on a terminal phase. Invalidating on every
      // "delta" fired a fresh get_context_telemetry IPC round-trip per streamed
      // token (the query has active observers), hammering the Tauri bridge during
      // long replies. The counts only meaningfully change once the turn lands.
      if (event.phase === "delta") return;
      qc.invalidateQueries({ queryKey: ["context-telemetry", selectedId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, selectedId]);

  useEffect(() => {
    previousOverflowCount.current = 0;
    autoOpenedContext.current = false;
  }, [selectedId]);

  useEffect(() => {
    const overflowCount = contextTelemetry.data?.overflowMessageCount ?? 0;
    // Auto-open the Context pane at most ONCE per chat when overflow first appears,
    // and never when the user has deliberately closed the tools rail (focus mode) —
    // yanking their pane open on every overflow tick feels like the app fighting
    // them. After the first surface, respect their layout choice.
    if (
      overflowCount > 0 &&
      previousOverflowCount.current === 0 &&
      !autoOpenedContext.current &&
      showUtilityPane
    ) {
      openUtilityPane("context");
      autoOpenedContext.current = true;
    }
    previousOverflowCount.current = overflowCount;
  }, [contextTelemetry.data?.overflowMessageCount, showUtilityPane]);

  // The workspace's linked folder (the repo) — independent of the active chat's
  // worktree that the backend swaps into the effective `workspaceRoot`. Use this
  // for workspace display (crumb, label) and link-state gating so a chat's worktree
  // path (`…/.ligus/worktrees/chat-<id>`) never shows as "the folder".
  const workspaceFolder = settings.data?.workspaceFolder ?? "";
  const activeWorkspace = workspaceList.data?.find((w) => w.active);
  const activeWorkspaceId = activeWorkspace?.id ?? "";
  // Settings and grant commands act on this workspace, the one on screen.
  setShownWorkspace(activeWorkspaceId);
  const activeWorkspaceName = activeWorkspace?.name?.trim();
  // The one banner: a lockdown of the open chat, else dearth mode.
  const lockdown = useQuery({
    queryKey: ["chat-lockdown", selectedId],
    queryFn: () => getChatLockdown(selectedId!),
    enabled: Boolean(selectedId),
    refetchInterval: 15000,
  });
  const dearth = useQuery({ queryKey: ["dearth-state"], queryFn: getDearthState, refetchInterval: 30000 });
  const dearthOn = Boolean(dearth.data?.enabled);
  const banner = blockingBanner({ lockdown: view === "workspace" && Boolean(lockdown.data?.active), dearth: dearthOn });
  // What waits on you: the Inbox lists it; the dock and tray count it.
  const needsYou = useNeedsYou(activeWorkspaceId);
  useDockBadge(needsYou);

  // Default view on workspace open/switch: land on the workspace's most-recent
  // chat so the chat-scoped panes (People, Agents, Review, …) populate instead of
  // an empty shell. Keyed on the active workspace id so it fires once per switch
  // (rail, palette, or initial load). Fetches chats FRESH via listChats — not from
  // a cache the switch path may not have invalidated (the rail only invalidates
  // "workspaces") — and keeps a still-valid manual selection, so it never yanks you
  // out of a no-chat Code/Diff view.
  const lastAutoSelectWs = useRef<string | null>(null);
  // Remembers the chat you last had open in each workspace (keyed by workspace id).
  const lastChatByWs = useRef<Map<string, string>>(new Map());
  // Default view on workspace open/switch. Returning to a workspace RESTORES the chat
  // you last had there (and thus its channel) DIRECTLY - without validating it against
  // list_chats, which returns a transient empty set right after a team-workspace switch
  // (the bug that reset to the placeholder). list_chats is consulted only for a genuine
  // first visit (no remembered chat), to land on the most-recent one.
  useEffect(() => {
    if (!activeWorkspaceId || view !== "workspace") return;
    const wsCtx = `${activeWorkspaceId}::${workspaceFolder}`;
    if (lastAutoSelectWs.current === wsCtx) return;
    lastAutoSelectWs.current = wsCtx;
    const wsId = activeWorkspaceId;
    const remembered = lastChatByWs.current.get(wsId);
    if (remembered) {
      setSelectedId(remembered);
      return;
    }
    let cancelled = false;
    void listChats()
      .then((all) => {
        if (cancelled) return;
        const live = all.filter((c) => !c.archived);
        const mostRecent = live.length
          ? [...live].sort(
              (a, b) => new Date(b.lastActivityAt).getTime() - new Date(a.lastActivityAt).getTime(),
            )[0]
          : null;
        if (!mostRecent) return; // no chats (or transient empty) - leave as-is
        lastChatByWs.current.set(wsId, mostRecent.id);
        setSelectedId((prev) => (prev && live.some((c) => c.id === prev) ? prev : mostRecent.id));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [activeWorkspaceId, workspaceFolder, view]);
  // Record a deliberate chat selection against the active workspace so it can be
  // restored on return. Called from the select handlers below.
  const rememberChat = useCallback(
    (id: string) => {
      if (activeWorkspaceId) lastChatByWs.current.set(activeWorkspaceId, id);
    },
    [activeWorkspaceId],
  );
  async function startNewChat() {
    try {
      const created = await createChat("");
      await qc.invalidateQueries({ queryKey: ["chats"] });
      rememberChat(created.id);
      setSelectedId(created.id);
      setView("workspace");
      setMode("chat");
    } catch (e) {
      toast.error(`Couldn't create chat: ${errMsg(e)}`);
    }
  }

  stepChatRef.current = (dir) => {
    // Follow what the sidebar shows (channels, filters, sort); fall back to
    // recency when the sidebar is hidden.
    let ids = Array.from(document.querySelectorAll<HTMLElement>("[data-chat-id]"))
      .map((el) => el.dataset.chatId!)
      .filter((id, i, all) => all.indexOf(id) === i);
    if (ids.length === 0) {
      const cached = qc.getQueryData<{ id: string; archived: boolean; lastActivityAt: string }[]>(["chats"]) ?? [];
      ids = cached
        .filter((c) => !c.archived)
        .sort((a, b) => new Date(b.lastActivityAt).getTime() - new Date(a.lastActivityAt).getTime())
        .map((c) => c.id);
    }
    if (ids.length === 0) return;
    const at = selectedId ? ids.indexOf(selectedId) : -1;
    const next = ids[(at + dir + ids.length) % ids.length] ?? ids[0];
    rememberChat(next);
    setSelectedId(next);
    setView("workspace");
    setMode("chat");
    document.querySelector<HTMLElement>(`[data-chat-id="${CSS.escape(next)}"]`)?.scrollIntoView({ block: "nearest" });
  };
  togglePaneRef.current = (pane) => {
    setView("workspace");
    if (showUtilityPane && utilityPane === pane) setShowUtilityPane(false);
    else openUtilityPane(pane);
  };

  // Cross-surface jumps (Flow board → chat, …): select the chat, then flash the
  // message if one was named.
  useEffect(() => {
    const onOpen = (e: Event) => {
      const d = (e as CustomEvent<OpenChatDetail>).detail;
      if (!d?.sessionId) return;
      rememberChat(d.sessionId);
      setSelectedId(d.sessionId);
      setView("workspace");
      setMode("chat");
      if (d.messageId) flashMessage(d.messageId);
    };
    window.addEventListener(OPEN_CHAT_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_CHAT_EVENT, onOpen);
  }, [rememberChat]);

  const workspaceLabel = useMemo(() => {
    // Prefer the workspace's own name (what you named the team/room); fall back
    // to the project-folder basename only when it has none.
    if (activeWorkspaceName) return activeWorkspaceName;
    const trimmed = workspaceFolder.replace(/[\\/]+$/, "");
    if (!trimmed) return "Ligus Workspace";
    const parts = trimmed.split(/[\\/]/);
    return parts[parts.length - 1] || "Ligus Workspace";
  }, [activeWorkspaceName, workspaceFolder]);

  const runtimeItems = runtimes.data ?? [];
  const currentRuntime =
    runtimeItems.find((rt) => rt.id === activeChat.data?.runtimeId) ?? runtimeItems[0] ?? null;

  async function refreshWorkspaceShell() {
    await Promise.all([
      qc.invalidateQueries({ queryKey: ["settings"] }),
      qc.invalidateQueries({ queryKey: ["chats"] }),
      qc.invalidateQueries({ queryKey: ["chat"] }),
      qc.invalidateQueries({ queryKey: ["runtimes"] }),
      qc.invalidateQueries({ queryKey: ["mcp"] }),
      qc.invalidateQueries({ queryKey: ["workspace-trust"] }),
      qc.invalidateQueries({ queryKey: ["vaults"] }),
      qc.invalidateQueries({ queryKey: ["skills"] }),
      qc.invalidateQueries({ queryKey: ["members"] }),
      qc.invalidateQueries({ queryKey: ["agents"] }),
      qc.invalidateQueries({ queryKey: ["proposals"] }),
      qc.invalidateQueries({ queryKey: ["diffs"] }),
      qc.invalidateQueries({ queryKey: ["mention-states"] }),
      qc.invalidateQueries({ queryKey: ["all-mention-states"] }),
      // Workspace-scoped but globally-keyed (no activeWorkspaceId in the key) — must
      // be invalidated on switch or the Activity/People panes show the previous
      // workspace's queued mentions / hosts / server roster until the next sync
      // (and workspace-hosts, which has no refetchInterval without a relay, never).
      qc.invalidateQueries({ queryKey: ["queued-work"] }),
      qc.invalidateQueries({ queryKey: ["workspace-hosts"] }),
      qc.invalidateQueries({ queryKey: ["server-members"] }),
    ]);
  }

  function openUtilityPane(pane: UtilityPane) {
    setUtilityPane(pane);
    setShowUtilityPane(true);
    // Asking for a pane leaves focus mode, so the pane actually shows.
    setFocusMode(false);
  }
  const showSidebar = sidebarVisible && !focusMode;
  const showRail = showUtilityPane && !focusMode;

  function startMenuResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const initialWidth = menuPaneWidth;

    const handleMove = (moveEvent: PointerEvent) => {
      const delta = moveEvent.clientX - startX;
      const nextMax = Math.min(
        MAX_MENU_PANE_WIDTH,
        window.innerWidth - utilityPaneWidth - MIN_CHAT_WIDTH,
      );
      setMenuPaneWidth(clampPaneWidth(initialWidth + delta, MIN_MENU_PANE_WIDTH, nextMax));
    };

    const stopResize = () => {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", stopResize);
    };

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", stopResize);
  }

  function startUtilityResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const initialWidth = utilityPaneWidth;

    const handleMove = (moveEvent: PointerEvent) => {
      const delta = moveEvent.clientX - startX;
      const nextMax = Math.min(
        MAX_UTILITY_PANE_WIDTH,
        window.innerWidth - menuPaneWidth - MIN_CHAT_WIDTH,
      );
      setUtilityPaneWidth(clampPaneWidth(initialWidth - delta, MIN_UTILITY_PANE_WIDTH, nextMax));
    };

    const stopResize = () => {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", stopResize);
    };

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", stopResize);
  }

  // A failed settings read must not fall through to the shell with empty
  // defaults (which reads as "onboarded, everything lost"). Show a recoverable
  // error shell that retries the load instead.
  if (settings.isError) {
    return (
      <div
        className="flex h-full flex-col items-center justify-center p-6"
        style={{ background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}
      >
        <ToastHost />
        <div
          className="w-full max-w-md rounded-2xl border p-6 text-center"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
        >
          <h1 className="text-lg font-semibold tracking-tight" style={{ color: "var(--ligus-danger-ink)" }}>
            Couldn't load your settings
          </h1>
          <p className="mt-2 text-sm opacity-70">
            Ligus couldn't read this workspace's configuration. Nothing has been lost — retry the
            load, or reload the app.
          </p>
          <div className="mt-4 flex items-center justify-center gap-2">
            <button
              onClick={() => void settings.refetch()}
              className="rounded-xl px-4 py-2 text-sm font-semibold transition-all hover:brightness-105"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
            >
              Retry
            </button>
            <button
              onClick={() => location.reload()}
              className="rounded-xl border px-4 py-2 text-sm font-medium transition-colors"
              style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
            >
              Reload
            </button>
          </div>
          <details className="mt-4 text-left">
            <summary className="cursor-pointer text-xs opacity-55">Error details</summary>
            <pre
              className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg p-3 text-[11px] leading-5"
              style={{ background: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
            >
              {errMsg(settings.error)}
            </pre>
          </details>
        </div>
      </div>
    );
  }

  if (needsOnboarding) {
    return (
      <Onboarding
        onComplete={async () => {
          // Throws into Onboarding, which shows the error on its last step.
          await markOnboarded();
          // Onboarding's appearance step persisted the theme + mode to
          // localStorage; sync App's state so its own applyTheme effect doesn't
          // revert to the mount-time default.
          setPalette(loadTheme());
          setAppearanceMode(loadMode());
          await qc.invalidateQueries({ queryKey: ["settings"] });
          refreshWorkspaceShell();
        }}
      />
    );
  }

  const tourSteps: TourStep[] = [
    {
      title: "Welcome to Ligus",
      body: "A 30-second tour of the workspace. Skip anytime — you can reopen this from the ⓘ button in the left rail, or ⌘K → “Take the tour.”",
    },
    {
      anchor: "workspaces",
      placement: "right",
      title: "Your workspaces",
      body: "Each workspace is a project — and, once you connect a relay, a team. Switch between them here, or use + to create or join one.",
      before: () => setView("workspace"),
    },
    {
      anchor: "sidebar",
      placement: "right",
      title: "Chats & agents",
      body: "Your chats live here. Start one, then @-mention agents to bring them in — @ligus always answers, and you can add your own agents on this or another machine.",
      before: () => setSidebarVisible(true),
    },
    {
      anchor: "canvas",
      placement: "bottom",
      title: "Talk to your agents",
      body: "This is the chat canvas. Ask @ligus to explain, review, or change code; it replies with proposed edits you stay in control of.",
      before: () => setMode("chat"),
    },
    {
      anchor: "canvas-tabs",
      placement: "bottom",
      ...TOUR_CANVAS_TABS,
    },
    {
      anchor: "canvas",
      placement: "bottom",
      ...TOUR_DIFF,
      before: () => setMode("diff"),
    },
    {
      anchor: "canvas",
      placement: "bottom",
      title: "A real code editor",
      body: "The Code tab is a full editor — file tree, integrated terminal, and language intelligence (hover, go-to-definition). Link a folder to a workspace to use it.",
      before: () => setMode("code"),
    },
    {
      anchor: "right-rail",
      placement: "left",
      title: "Tools, workflows & context",
      body: "The right rail carries the agent roster and their hosts, multi-stage workflows, live context usage, and logs.",
      before: () => {
        setMode("chat");
        setShowUtilityPane(true);
      },
    },
    {
      anchor: "help",
      placement: "right",
      title: "You’re all set",
      body: "That’s the tour. Reopen it anytime from this ⓘ button, or press ⌘K and search “tour.” Happy building.",
      before: () => setShowUtilityPane(tourSnapshot.current?.utility ?? false),
    },
  ];

  return (
    <div
      className="flex h-full min-w-0 overflow-hidden"
      style={{ background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}
    >
      {tourOpen && <Tour steps={tourSteps} onClose={() => endTour()} />}
      <ToastHost />
      <DialogHost />
      <PresenceSheet />
      <PhoneEnrollTripwire />
      {shortcutsOpen && <ShortcutsSheet onClose={() => setShortcutsOpen(false)} />}
      {errorsOpen && <ErrorLogSheet onClose={() => setErrorsOpen(false)} />}
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        sessionId={selectedId}
        workspaceId={activeWorkspaceId}
        actions={{
          newChat: startNewChat,
          openSettings: () => openSettings(),
          selectChat: (id) => {
            rememberChat(id);
            setSelectedId(id);
            setView("workspace");
          },
          selectWorkspace: async (id) => {
            await setActiveWorkspace(id);
            await qc.invalidateQueries({ queryKey: ["workspaces"] });
            await qc.invalidateQueries({ queryKey: ["chats"] });
            // The code dir follows the active workspace — refetch settings so the
            // editor/tree/diff/terminal pick up the new (or empty) workspaceRoot.
            await qc.invalidateQueries({ queryKey: ["settings"] });
            setView("workspace");
          },
          toggleSidebar: () => setSidebarVisible((v) => !v),
          toggleTools: () => setShowUtilityPane((v) => !v),
          setCanvasMode: (m) => {
            setView("workspace");
            setMode(m);
          },
          quickOpenFile: () => {
            setView("workspace");
            setMode("code");
            setCodePending("quick-open");
          },
          newTerminal: () => {
            setView("workspace");
            setMode("code");
            setCodePending("new-terminal");
          },
          openFriends: () => setView("friends"),
          openPane: (pane) => {
            setView("workspace");
            openUtilityPane(pane);
          },
          openSettingsTab: (tab) => openSettings(tab),
          cycleAppearance: () =>
            setAppearanceMode(appearanceMode === "dark" ? "light" : "dark"),
          takeTour: () => startTour(),
          admitBox: activeWorkspace?.kind === "room" ? () => setAdmitBoxOpen(true) : undefined,
          stepChat: (dir) => stepChatRef.current(dir),
          openProposal: (sessionId) => {
            rememberChat(sessionId);
            setSelectedId(sessionId);
            setView("workspace");
            setMode("chat");
            openUtilityPane("review");
          },
          mentionAgent: (name) => {
            setView("workspace");
            setMode("chat");
            setComposerSeed(`@${name} `);
            window.setTimeout(() => document.querySelector<HTMLTextAreaElement>("textarea[data-composer]")?.focus(), 0);
          },
          addPhone: () => requestAddPhone(),
          showShortcuts: () => setShortcutsOpen(true),
          showErrors: () => setErrorsOpen(true),
          openInbox: () => setView("inbox"),
          toggleFocusMode: () => setFocusMode((v) => !v),
          // After the palette closes, so its focus-return doesn't steal it back.
          focusComposer: () =>
            window.setTimeout(() => document.querySelector<HTMLTextAreaElement>("textarea[data-composer]")?.focus(), 0),
        }}
      />
      {!focusMode && (
      <WorkspaceRail
        onJoinRoom={() => setAddWsOpen(true)}
        onOpenFriends={() => setView("friends")}
        sidebarVisible={sidebarVisible}
        onToggleSidebar={() => setSidebarVisible((v) => !v)}
        onOpenSettings={() => openSettings()}
        onOpenHelp={() => startTour()}
        settingsActive={settingsOpen}
      />
      )}
      <AddWorkspaceModal open={addWsOpen} onClose={() => setAddWsOpen(false)} />
      {showSidebar && (
      <Sidebar
        width={menuPaneWidth}
        selectedId={selectedId}
        sessionId={selectedId}
        view={view}
        workspaceLabel={workspaceLabel}
        workspacePath={workspaceFolder}
        activeWorkspaceId={activeWorkspaceId}
        knownWorkspaces={settings.data?.knownWorkspaces ?? []}
        displayName={settings.data?.displayName ?? "You"}
        utilityPane={utilityPane}
        onAdmitBox={activeWorkspace?.kind === "room" ? () => setAdmitBoxOpen(true) : undefined}
        onSelect={(id) => {
          rememberChat(id);
          setSelectedId(id);
          setView("workspace");
          setMode("chat");
        }}
        onOpenSettings={() => openSettings()}
        onAddWorkspace={async () => {
          const path = await pickWorkspaceFolder();
          if (!path) return;
          await addWorkspaceToList(path);
          await qc.invalidateQueries({ queryKey: ["settings"] });
        }}
        onRemoveWorkspace={async (path) => {
          await removeWorkspaceFromList(path);
          await qc.invalidateQueries({ queryKey: ["settings"] });
        }}
        onSwitchWorkspace={async (path) => {
          await setWorkspaceRoot(path);
          // Don't blank the selection — the default-view effect restores the chat
          // (and channel) you last had open in this workspace once it re-fires.
          setMode("chat");
          setUtilityPane("tools");
          await refreshWorkspaceShell();
          setView("workspace");
        }}
        onOpenUtilityPane={(pane) => {
          setView("workspace");
          openUtilityPane(pane);
        }}
        inboxCount={needsYouCount(needsYou)}
        onOpenInbox={() => setView((v) => (v === "inbox" ? "workspace" : "inbox"))}
      />
      )}

      {showSidebar && (
        <PaneResizeHandle onPointerDown={startMenuResize} ariaLabel="Resize menu and chat panes" />
      )}

      <main data-tour="canvas" className="flex min-w-0 flex-1 flex-col overflow-hidden">
        {banner === "dearth" && <DearthBanner onManage={() => openSettings("guards")} />}
        {view === "inbox" ? (
          <PaneErrorBoundary label="Inbox">
            <Inbox
              items={needsYou}
              actions={{
                openChat: (id) => {
                  rememberChat(id);
                  setSelectedId(id);
                  setView("workspace");
                  setMode("chat");
                },
                openProposal: (sessionId) => {
                  rememberChat(sessionId);
                  setSelectedId(sessionId);
                  setView("workspace");
                  setMode("chat");
                  openUtilityPane("review");
                },
                openSettingsTab: (tab) => openSettings(tab),
              }}
            />
          </PaneErrorBoundary>
        ) : view === "friends" ? (
          <PaneErrorBoundary label="Friends">
            <FriendsView
              onOpenSettings={(tab) => openSettings(tab)}
              onOpenDm={() => {
                setSelectedId(null);
                setMode("chat");
                setView("workspace");
              }}
            />
          </PaneErrorBoundary>
        ) : !selectedId ? (
          <>
            {/* No chat selected, but Diff and Code are workspace-scoped, so the
                canvas stays reachable. A minimal header carries the mode tabs
                (and the workspace crumb); Chat falls back to the placeholder. */}
            <div
              className="flex items-center gap-3 border-b px-4 py-2"
              style={{ borderColor: "var(--ligus-line)" }}
            >
              <div className="flex min-w-0 flex-1 items-center gap-1.5">
                <span className="truncate text-base font-semibold tracking-tight opacity-70">
                  {workspaceLabel}
                </span>
              </div>
              {focusMode && (
                <FocusControls
                  needsYou={needsYouCount(needsYou)}
                  onExit={() => setFocusMode(false)}
                  onOpenInbox={() => {
                    setFocusMode(false);
                    setView("inbox");
                  }}
                />
              )}
              <CanvasModeTabs mode={mode} onChangeMode={setMode} />
            </div>

            <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
              <div className="min-w-0 flex-1">
                <PaneErrorBoundary label="This view">
                  {(mode === "diff" || mode === "code") && !workspaceFolder ? (
                    <BindWorkspacePrompt />
                  ) : mode === "diff" ? (
                    <Suspense
                      fallback={
                        <CanvasLoading label="Loading the diff view" />
                      }
                    >
                      <DiffView
                        pendingPath={pendingDiffPath}
                        onConsumePendingPath={() => setPendingDiffPath(null)}
                      />
                    </Suspense>
                  ) : mode === "code" ? (
                    <Suspense
                      fallback={
                        <CanvasLoading label="Loading the code editor" />
                      }
                    >
                      <CodeView
                        pending={codePending}
                        onConsumePending={() => setCodePending(null)}
                      />
                    </Suspense>
                  ) : (
                    <EmptyCanvas
                      workspaceName={workspaceLabel}
                      onNewChat={() => void startNewChat()}
                      onOpenPalette={() => setPaletteOpen(true)}
                    />
                  )}
                </PaneErrorBoundary>
              </div>
            </div>
          </>
        ) : (
          <>
            <ChatHeaderBar
              title={activeChat.data?.title ?? "New Chat"}
              runsOn={
                selectedId ? (
                  <ChatRunsOn
                    sessionId={selectedId}
                    runtime={currentRuntime ? runtimePickerLabel(currentRuntime) : null}
                    runtimeDto={currentRuntime}
                  />
                ) : undefined
              }
              contextPct={
                contextTelemetry.data
                  ? Math.min(
                      100,
                      Math.round(
                        ((contextTelemetry.data.systemPromptTokens +
                          contextTelemetry.data.keptTokens) /
                          Math.max(1, contextTelemetry.data.contextWindowTokens)) *
                          100,
                      ),
                    )
                  : null
              }
              onOpenContext={() => openUtilityPane("context")}
              workspaceCrumb={showSidebar ? undefined : workspaceLabel}
              focus={focusMode ? { needsYou: needsYouCount(needsYou), onExit: () => setFocusMode(false), onOpenInbox: () => { setFocusMode(false); setView("inbox"); } } : undefined}
              mode={mode}
              onChangeMode={setMode}
              utilityPaneVisible={showRail}
              onToggleTools={() => {
                if (showUtilityPane) {
                  setShowUtilityPane(false);
                  return;
                }
                openUtilityPane("tools");
              }}
              onRename={async () => {
                if (!selectedId) return;
                const next = await promptDialog("Rename chat", {
                  placeholder: "Chat title",
                  defaultValue: activeChat.data?.title ?? "",
                });
                if (next === null || !next.trim()) return;
                try {
                  await renameChat(selectedId, next.trim());
                  await qc.invalidateQueries({ queryKey: ["chat", selectedId] });
                  await qc.invalidateQueries({ queryKey: ["chats"] });
                } catch (e) {
                  toast.error(`Couldn't rename chat: ${errMsg(e)}`);
                }
              }}
            />
            {selectedId && banner === "lockdown" && <LockdownBar sessionId={selectedId} />}

            <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
              <div className="min-w-0 flex-1">
                <PaneErrorBoundary label="This view">
                <>
                {mode === "chat" && (
                  <ChatView
                    sessionId={selectedId}
                    runtimes={runtimeItems}
                    currentRuntimeId={activeChat.data?.runtimeId ?? currentRuntime?.id ?? ""}
                    onOpenTools={() => openUtilityPane("tools")}
                    pendingInsert={composerSeed}
                    onConsumePendingInsert={() => setComposerSeed(null)}
                  />
                )}
                {(mode === "diff" || mode === "code") && !workspaceFolder && (
                  <BindWorkspacePrompt />
                )}
                {mode === "diff" && !!workspaceFolder && (
                  <Suspense
                    fallback={
                      <CanvasLoading label="Loading the diff view" />
                    }
                  >
                    <DiffView
                      pendingPath={pendingDiffPath}
                      onConsumePendingPath={() => setPendingDiffPath(null)}
                    />
                  </Suspense>
                )}
                {mode === "code" && !!workspaceFolder && (
                  <Suspense
                    fallback={
                      <CanvasLoading label="Loading the code editor" />
                    }
                  >
                    <CodeView
                      pending={codePending}
                      onConsumePending={() => setCodePending(null)}
                    />
                  </Suspense>
                )}
                </>
                </PaneErrorBoundary>
              </div>

              {showRail && (
                <>
                  <PaneResizeHandle
                    onPointerDown={startUtilityResize}
                    ariaLabel="Resize chat and utility panes"
                  />

                  <PaneErrorBoundary label="This panel">
                    <RightRail
                      width={utilityPaneWidth}
                      sessionId={selectedId}
                      pane={utilityPane}
                      activeRuntimeId={activeChat.data?.runtimeId ?? currentRuntime?.id ?? ""}
                      onChangePane={setUtilityPane}
                      onClose={() => setShowUtilityPane(false)}
                      // The pane's own icon rail duplicates the sidebar's
                      // Workspace list (F10) — only show it when the sidebar is
                      // hidden, so panes stay reachable without doubling nav.
                      showPaneRail={!sidebarVisible}
                    />
                  </PaneErrorBoundary>
                </>
              )}
            </div>
          </>
        )}
        {/* State that doesn't need you lives here, at the foot of the canvas;
            only a blocking state (Lockdown, Dearth) gets the one banner up top. */}
        <AppStatusBar
          onOpenSettings={(tab) => openSettings(tab)}
          onOpenReview={() => {
            setView("workspace");
            openUtilityPane("review");
          }}
          onShowErrors={() => setErrorsOpen(true)}
          hidden={focusMode}
          inboxCount={showSidebar ? 0 : needsYouCount(needsYou)}
          onOpenInbox={() => setView("inbox")}
          dearthAside={dearthOn && banner !== "dearth"}
        />
      </main>

      {/* Settings presents OVER the workspace — the chat behind it stays mounted.
          The nonce remounts the sheet so a repeat open re-applies the tab. */}
      {settingsOpen && (
        <SettingsView
          key={`settings-${settingsNonce}`}
          initialTab={settingsTab}
          palette={palette}
          onPaletteChange={setPalette}
          appearanceMode={appearanceMode}
          onAppearanceModeChange={setAppearanceMode}
          onClose={() => setSettingsOpen(false)}
          activeChat={selectedId ? { id: selectedId, title: activeChat.data?.title ?? "" } : null}
        />
      )}
      {/* After Settings so it stacks above it when opened from there. */}
      {admitBoxOpen && activeWorkspace?.kind === "room" && (
        <AdmitBoxModal workspaceId={activeWorkspace.id} workspaceName={activeWorkspace.name} onClose={() => setAdmitBoxOpen(false)} />
      )}
    </div>
  );
}

function PaneResizeHandle({
  onPointerDown,
  ariaLabel,
}: {
  onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  ariaLabel: string;
}) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={ariaLabel}
      className="relative w-1.5 shrink-0 cursor-col-resize bg-transparent transition-colors hover:bg-[color:var(--ligus-overlay)] active:bg-[color:var(--ligus-overlay)]"
      style={{ touchAction: "none" }}
      onPointerDown={onPointerDown}
    >
      <span className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-[color:var(--ligus-line)]" />
    </div>
  );
}

/// The Chat / Diff / Code canvas-mode switch. Extracted so both the chat header
/// and the no-chat workspace header render the same control (single tabs array).
/// Diff and Code are workspace-scoped, so this stays meaningful with no chat
/// selected.
function CanvasModeTabs({
  mode,
  onChangeMode,
}: {
  mode: CanvasMode;
  onChangeMode: (m: CanvasMode) => void;
}) {
  const tabs: { id: CanvasMode; label: string }[] = [
    { id: "chat", label: "Chat" },
    { id: "diff", label: "Diff" },
    { id: "code", label: "Code" },
  ];
  return (
    <div
      data-tour="canvas-tabs"
      className="flex shrink-0 items-center gap-0.5 rounded-xl border p-0.5"
      style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
      role="tablist"
      aria-label="Canvas mode"
    >
      {tabs.map((t) => (
        <button
          key={t.id}
          role="tab"
          title={CANVAS_TAB_HINT[t.id]}
          aria-selected={mode === t.id}
          onClick={() => onChangeMode(t.id)}
          className="rounded-lg px-3 py-1 text-sm font-medium transition-colors hover:text-ink"
          style={{
            background: mode === t.id ? "var(--ligus-panel)" : "transparent",
            color: mode === t.id ? "var(--ligus-ink)" : "var(--ligus-ink-soft)",
            boxShadow: mode === t.id ? "var(--ligus-shadow-1)" : undefined,
          }}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

/// In focus mode, the header's only extra: how many things need you (one
/// quiet badge that opens the Inbox) and a way back.
function FocusControls({ needsYou, onExit, onOpenInbox }: { needsYou: number; onExit: () => void; onOpenInbox: () => void }) {
  return (
    <div className="flex shrink-0 items-center gap-1 text-xs text-ink-soft" data-testid="focus-controls">
      {needsYou > 0 && (
        <button
          type="button"
          onClick={onOpenInbox}
          title="Open the Inbox"
          className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 hover:bg-[color:var(--ligus-hover)]"
        >
          <span aria-hidden style={{ color: "var(--ligus-state-needs-you, var(--ligus-warm-ink))" }}>
            <IconStateNeedsYou size={11} />
          </span>
          {needsYouLabel(needsYou)}
        </button>
      )}
      <button
        type="button"
        onClick={onExit}
        title={`Leave focus mode (${keyLabel(shortcutKey("focusMode")!)})`}
        className="rounded-full px-2 py-0.5 hover:bg-[color:var(--ligus-hover)]"
      >
        Leave focus
      </button>
    </div>
  );
}

/// Single compact chat header: title + rename, where the chat runs, the
/// Chat/Diff mode tabs, and the Tools toggle. Sync state lives in the status
/// bar at the foot of the canvas.
function ChatHeaderBar({
  title,
  runsOn,
  mode,
  onChangeMode,
  utilityPaneVisible,
  onToggleTools,
  onRename,
  contextPct,
  onOpenContext,
  workspaceCrumb,
  focus,
}: {
  title: string;
  /// One quiet line under the title: where the chat runs.
  runsOn?: ReactNode;
  mode: CanvasMode;
  onChangeMode: (m: CanvasMode) => void;
  utilityPaneVisible: boolean;
  onToggleTools: () => void;
  onRename: () => void;
  /// The active workspace name, shown as a crumb before the chat title when the
  /// sidebar is collapsed — otherwise "which workspace am I in?" is invisible.
  workspaceCrumb?: string;
  /// Percent of the model's context window the next reply will use (null
  /// until telemetry loads). Clicking the pill opens the Context pane.
  contextPct: number | null;
  onOpenContext: () => void;
  /// Focus mode is on: one quiet needs-you badge and a way out.
  focus?: { needsYou: number; onExit: () => void; onOpenInbox: () => void };
}) {
  return (
    <div
      className="flex items-center gap-3 border-b px-4 py-2"
      style={{ borderColor: "var(--ligus-line)" }}
    >
      <div className="flex min-w-0 flex-1 flex-col">
      <div className="flex min-w-0 items-center gap-1.5">
        {workspaceCrumb && (
          <>
            <span className="shrink-0 truncate text-base font-semibold tracking-tight opacity-55">
              {workspaceCrumb}
            </span>
            <span className="shrink-0 opacity-30" aria-hidden>/</span>
          </>
        )}
        <h1 className="truncate text-base font-semibold tracking-tight">{title}</h1>
        <button
          className="shrink-0 rounded-md p-1 text-ink-faint transition-colors hover:bg-[color:var(--ligus-hover)] hover:text-ink"
          title="Rename chat"
          aria-label="Rename chat"
          onClick={onRename}
        >
          <IconPencil size={13} />
        </button>
      </div>
      {runsOn}
      </div>

      {focus && <FocusControls {...focus} />}

      {contextPct !== null && (
        <button
          onClick={onOpenContext}
          title={`${contextPct}% of the model's context window in use — open the Context pane`}
          aria-label="Context usage"
          className="flex shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-xs transition-colors hover:border-[color:var(--ligus-accent-cool)]"
          style={{
            borderColor: "var(--ligus-line)",
            background: "var(--ligus-mist)",
            color: contextPct >= 80 ? "var(--ligus-warn-ink)" : "var(--ligus-ink)",
          }}
        >
          <IconHexagon size={11} />
          {contextPct}% context
        </button>
      )}

      <CanvasModeTabs mode={mode} onChangeMode={onChangeMode} />

      <button
        onClick={onToggleTools}
        aria-pressed={utilityPaneVisible}
        className="shrink-0 rounded-xl border px-3 py-1 text-sm font-medium transition-colors"
        style={{
          borderColor: utilityPaneVisible
            ? "color-mix(in srgb, var(--ligus-accent-cool) 40%, transparent)"
            : "var(--ligus-line)",
          background: utilityPaneVisible
            ? "color-mix(in srgb, var(--ligus-accent-cool) 18%, transparent)"
            : "var(--ligus-mist)",
          color: "var(--ligus-ink)",
        }}
      >
        Tools
      </button>
    </div>
  );
}
