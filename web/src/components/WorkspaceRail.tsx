import { useEffect, useMemo, useRef, useState } from "react";
import { copyWhenReady } from "@/lib/clipboard";
import { inviteError } from "@/lib/invites";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listWorkspaces,
  listAllMentionStates,
  setActiveWorkspace,
  setWorkspaceIcon,
  syncStatus,
  workspaceInvite,
  workspaceShareCode,
  removeWorkspace,
  leaveWorkspace,
  leaveWorkspaceCheck,
  type WorkspaceInfoDto,
} from "@/lib/ipc";
import { LigusBrandMark } from "@/components/LigusBrand";
import { AdmitBoxModal } from "@/components/AdmitBoxModal";
import { MenuPanel, type MenuItemSpec } from "@/components/Menu";
import { toast, errMsg } from "@/components/Toast";
import { confirmDialog } from "@/components/Dialog";
import { IconPlus, IconUsers, IconPanelLeft, IconGear, IconInfo } from "@/lib/icons";

/// Discord-style rail of workspace "servers": Personal (local) pinned at top,
/// then each joined team room, then a ＋ to create/join one. Click selects
/// (scopes the chat list); right-click a team opens a menu to share or leave it.
export function WorkspaceRail({
  onJoinRoom,
  onOpenFriends,
  sidebarVisible,
  onToggleSidebar,
  onOpenSettings,
  onOpenHelp,
  settingsActive,
}: {
  onJoinRoom: () => void;
  onOpenFriends?: () => void;
  /// Focus mode: collapse/restore the sidebar (also ⌘B).
  sidebarVisible?: boolean;
  onToggleSidebar?: () => void;
  /// Settings lives at the rail's bottom (VS Code's activity-bar pattern) so
  /// it stays reachable when the sidebar is dismissed.
  onOpenSettings?: () => void;
  /// Relaunch the product tour (also the tour's own final-step anchor).
  onOpenHelp?: () => void;
  settingsActive?: boolean;
}) {
  const qc = useQueryClient();
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const workspaces = useQuery({
    queryKey: ["workspaces", sync.data?.relayConfigured, sync.data?.room],
    queryFn: listWorkspaces,
  });
  // Cross-workspace self-mentions: light a dot on any *background* workspace
  // holding an unread mention. Read state is the same per-session localStorage
  // cursor the Sidebar maintains (`ligus.read.<sessionId>` = message count at
  // last open); a mention past that cursor is unread.
  const mentions = useQuery({ queryKey: ["all-mention-states"], queryFn: listAllMentionStates });
  // Read cursors live in localStorage, not React state, so a mention being read
  // (Sidebar writes `ligus.read.*`) wouldn't otherwise recompute the memo below.
  // Bump on the Sidebar's "ligus:read-cursor" event so the badge clears promptly.
  const [cursorTick, setCursorTick] = useState(0);
  useEffect(() => {
    const onCursor = () => setCursorTick((t) => t + 1);
    window.addEventListener("ligus:read-cursor", onCursor);
    return () => window.removeEventListener("ligus:read-cursor", onCursor);
  }, []);
  const unreadWorkspaces = useMemo(() => {
    const set = new Set<string>();
    for (const m of mentions.data ?? []) {
      const cursor = Number(window.localStorage.getItem(`ligus.read.${m.sessionId}`)) || 0;
      if (m.lastMentionOrdinal > cursor) set.add(m.workspaceId);
    }
    return set;
  }, [mentions.data, cursorTick]);
  const [menu, setMenu] = useState<{ ws: WorkspaceInfoDto; x: number; y: number } | null>(null);
  const [admitting, setAdmitting] = useState<WorkspaceInfoDto | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const iconTarget = useRef<string | null>(null);

  function chooseIcon(ws: WorkspaceInfoDto) {
    iconTarget.current = ws.id;
    fileInput.current?.click();
  }

  async function onIconFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-picking the same file
    const wsId = iconTarget.current;
    iconTarget.current = null;
    if (!file || !wsId) return;
    try {
      const dataUrl = await fileToIconDataUrl(file);
      await setWorkspaceIcon(wsId, dataUrl);
      await refresh();
      toast.success("Workspace icon updated.");
    } catch (err) {
      toast.error(errMsg(err));
    }
  }

  function removeIcon(ws: WorkspaceInfoDto) {
    void (async () => {
      try {
        await setWorkspaceIcon(ws.id, null);
        await refresh();
        toast.success("Workspace icon removed.");
      } catch (err) {
        toast.error(errMsg(err));
      }
    })();
  }

  async function select(id: string) {
    try {
      await setActiveWorkspace(id);
      await qc.invalidateQueries({ queryKey: ["workspaces"] });
      await qc.invalidateQueries({ queryKey: ["chats"] });
      await qc.invalidateQueries({ queryKey: ["all-mention-states"] });
      // The code dir follows the active workspace — refetch settings so the
      // editor/tree/diff/terminal pick up the new (or empty) workspaceRoot.
      await qc.invalidateQueries({ queryKey: ["settings"] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function refresh() {
    await qc.invalidateQueries({ queryKey: ["workspaces"] });
    await qc.invalidateQueries({ queryKey: ["chats"] });
    await qc.invalidateQueries({ queryKey: ["sync-status"] });
  }

  async function shareShortCode(ws: WorkspaceInfoDto) {
    try {
      const share = workspaceShareCode(ws.id);
      const code = await copyWhenReady(share.then((s) => s.code));
      const { expiresIn } = await share;
      toast.success(`Short code ${code} copied — expires in ${Math.round(expiresIn / 60)} min.`);
    } catch (e) {
      const m = inviteError(e);
      toast.error(m.message, m.action);
    }
  }
  async function copyInvite(ws: WorkspaceInfoDto) {
    try {
      // Started inside the click (WebKit); a refused invite copies nothing.
      await copyWhenReady(workspaceInvite(ws.id));
      toast.success(`Invite for “${ws.name}” copied.`);
    } catch (e) {
      const m = inviteError(e);
      toast.error(m.message, m.action);
    }
  }
  function workspaceMenuItems(ws: WorkspaceInfoDto): MenuItemSpec[] {
    const items: MenuItemSpec[] = [{ type: "header", label: ws.name }, { label: "Set icon…", onClick: () => chooseIcon(ws) }];
    if (ws.iconUrl) items.push({ label: "Remove icon", onClick: () => void removeIcon(ws) });
    if (ws.kind === "room") {
      items.push(
        { label: "Share short code", onClick: () => void shareShortCode(ws) },
        { label: "Copy invite", onClick: () => void copyInvite(ws) },
        { label: "Admit a box…", onClick: () => setAdmitting(ws) },
        { type: "separator" },
        // leave() asks for confirmation itself.
        { label: "Leave workspace", danger: true, onClick: () => void leave(ws) },
      );
    }
    return items;
  }

  /// Leave on your own (workspace model v3, 10.4): one line on what
  /// happens, and that leaving isn't revoking. Where the workspace refuses
  /// it (the founder, the last Owner, a linked computer), the only offer is
  /// removing it from this device.
  async function leave(ws: WorkspaceInfoDto) {
    let check;
    try {
      check = await leaveWorkspaceCheck(ws.id);
    } catch (e) {
      toast.error(errMsg(e));
      return;
    }
    if (check.state === "refused") {
      const ok = await confirmDialog(
        `${check.refusal ?? "This workspace doesn't let you leave from here."} You can remove “${ws.name}” from this device only: you stay a member and its chats stop syncing here.`,
        { title: "Couldn't leave", confirmLabel: "Remove from this device", danger: true },
      );
      if (!ok) return;
      try {
        await removeWorkspace(ws.id);
        await refresh();
        toast.success(`Removed “${ws.name}” from this device.`);
      } catch (e) {
        toast.error(errMsg(e));
      }
      return;
    }
    const ok = await confirmDialog(`${check.line} ${check.reminder}`, {
      title: `Leave “${ws.name}”?`,
      confirmLabel: check.state === "passkey" ? "Leave with passkey" : "Leave",
      danger: true,
    });
    if (!ok) return;
    try {
      await leaveWorkspace(ws.id);
      await refresh();
      toast.success(`Left “${ws.name}”.`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  return (
    <div
      data-tour="workspaces"
      className="flex w-14 shrink-0 flex-col items-center gap-2 border-r py-3"
      style={{ background: "var(--ligus-sidebar-top)", borderColor: "var(--ligus-line)" }}
    >
      {(workspaces.data ?? []).map((w) => {
        const local = w.kind === "local";
        // Flag a background workspace holding an unread self-mention (the active
        // one already surfaces mentions in its own sidebar).
        const hasMention = !w.active && unreadWorkspaces.has(w.id);
        return (
          <div key={w.id} className="relative flex w-full items-center justify-center">
            {/* Leading-edge pip so the active workspace reads even at a glance,
                not only by fill (§7.2). */}
            <span
              className="absolute left-0 rounded-r-full transition-all"
              style={{
                width: 3,
                height: w.active ? 22 : 0,
                background: "var(--ligus-accent-cool)",
              }}
              aria-hidden
            />
            <button
              onClick={() => select(w.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setMenu({ ws: w, x: e.clientX, y: e.clientY });
              }}
              onKeyDown={(e) => {
                // The keyboard's menu key (or Shift+F10) opens the same menu.
                if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                  e.preventDefault();
                  const r = e.currentTarget.getBoundingClientRect();
                  setMenu({ ws: w, x: r.right, y: r.top });
                }
              }}
              data-tip={local ? "Personal — right-click to set an icon" : `${w.name} — right-click for options`}
              aria-current={w.active ? "true" : undefined}
              aria-label={w.name}
              className="flex h-10 w-10 items-center justify-center overflow-hidden text-sm font-semibold transition-all hover:brightness-110"
              style={{
                background: w.active ? "var(--ligus-accent-fill)" : "var(--ligus-panel)",
                color: w.active ? "var(--ligus-on-accent)" : "var(--ligus-ink)",
                border: w.active ? "none" : "1px solid var(--ligus-line)",
                borderRadius: w.active ? 12 : 18,
              }}
            >
              {w.iconUrl ? (
                <img src={w.iconUrl} alt="" className="h-full w-full object-cover" />
              ) : local ? (
                <LigusBrandMark size={22} />
              ) : (
                <span>{roomInitials(w.name)}</span>
              )}
            </button>
            {hasMention && (
              <span
                className="absolute right-0 top-0 h-2.5 w-2.5 rounded-full"
                style={{
                  background: "var(--ligus-accent-cool)",
                  boxShadow: "0 0 0 2px var(--ligus-sidebar-top)",
                }}
                aria-label={`${w.name} — unread mention`}
                data-tip={`${w.name} — you were mentioned`}
              />
            )}
          </div>
        );
      })}
      <button
        onClick={onJoinRoom}
        data-tip="Add a workspace — create or join a team"
        aria-label="Add a workspace"
        className="mt-1 flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110"
        style={{ background: "var(--ligus-panel)", color: "var(--ligus-ink)", border: "1px solid var(--ligus-line)", opacity: 0.85 }}
      >
        <IconPlus size={18} />
      </button>

      {onOpenFriends && (
        <button
          onClick={onOpenFriends}
          data-tip="People — co-members, direct messages and invites"
          aria-label="People"
          className="mt-auto flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110"
          style={{ background: "var(--ligus-panel)", color: "var(--ligus-ink)", border: "1px solid var(--ligus-line)", opacity: 0.85 }}
        >
          <IconUsers size={17} />
        </button>
      )}

      {onToggleSidebar && (
        <button
          onClick={onToggleSidebar}
          data-tip={`${sidebarVisible ? "Hide" : "Show"} sidebar (⌘B)`}
          aria-label={`${sidebarVisible ? "Hide" : "Show"} sidebar`}
          aria-pressed={!sidebarVisible}
          className={`flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110 ${onOpenFriends ? "" : "mt-auto"}`}
          style={{
            background: "var(--ligus-panel)",
            color: "var(--ligus-ink)",
            border: "1px solid var(--ligus-line)",
            opacity: sidebarVisible ? 0.85 : 1,
          }}
        >
          <IconPanelLeft size={16} />
        </button>
      )}

      {onOpenHelp && (
        <button
          data-tour="help"
          onClick={onOpenHelp}
          data-tip="Take the tour — a quick walk through Ligus"
          aria-label="Take the tour"
          className={`flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110 ${onOpenFriends || onToggleSidebar ? "" : "mt-auto"}`}
          style={{ background: "var(--ligus-panel)", color: "var(--ligus-ink)", border: "1px solid var(--ligus-line)", opacity: 0.85 }}
        >
          <IconInfo size={17} />
        </button>
      )}

      {onOpenSettings && (
        <button
          onClick={onOpenSettings}
          data-tip="Settings"
          aria-label="Settings"
          aria-pressed={settingsActive}
          className={`flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110 ${onOpenFriends || onToggleSidebar || onOpenHelp ? "" : "mt-auto"}`}
          style={{
            background: settingsActive ? "var(--ligus-accent-fill)" : "var(--ligus-panel)",
            color: settingsActive ? "var(--ligus-on-accent)" : "var(--ligus-ink)",
            border: settingsActive ? "none" : "1px solid var(--ligus-line)",
            opacity: settingsActive ? 1 : 0.85,
          }}
        >
          <IconGear size={17} />
        </button>
      )}

      {menu && (
        <MenuPanel
          label={`${menu.ws.name} actions`}
          placement={{ at: { x: menu.x + 4, y: menu.y } }}
          onClose={() => setMenu(null)}
          items={workspaceMenuItems(menu.ws)}
        />
      )}

      {admitting && <AdmitBoxModal workspaceId={admitting.id} workspaceName={admitting.name} onClose={() => setAdmitting(null)} />}

      <input
        ref={fileInput}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
        className="hidden"
        onChange={(e) => void onIconFile(e)}
      />
    </div>
  );
}

/// Read an image file and downscale it to a small square PNG data URL so the
/// rail glyph stays tiny in settings (we cap the backend at ~512 KB anyway).
/// SVGs are passed through as-is (they're already small and scale crisply).
export async function fileToIconDataUrl(file: File, size = 128): Promise<string> {
  const readAsDataUrl = () =>
    new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(reader.error ?? new Error("could not read file"));
      reader.readAsDataURL(file);
    });

  const raw = await readAsDataUrl();
  if (file.type === "image/svg+xml") return raw;

  const img = new Image();
  await new Promise<void>((resolve, reject) => {
    img.onload = () => resolve();
    img.onerror = () => reject(new Error("could not decode image"));
    img.src = raw;
  });

  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d");
  if (!ctx) return raw;
  // Cover-crop to a centered square so non-square uploads aren't distorted.
  const side = Math.min(img.width, img.height);
  const sx = (img.width - side) / 2;
  const sy = (img.height - side) / 2;
  ctx.drawImage(img, sx, sy, side, side, 0, 0, size, size);
  return canvas.toDataURL("image/png");
}

function roomInitials(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "#";
  const parts = trimmed.split(/[\s\-_]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return trimmed.slice(0, 2).toUpperCase();
}
