import { useState } from "react";
import { copyWhenReady } from "@/lib/clipboard";
import { inviteError } from "@/lib/invites";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listWorkspaces,
  createWorkspace,
  joinWorkspace,
  workspaceInvite,
  removeWorkspace,
  workspaceShareCode,
  redeemShortCode,
  passkeyRegister,
  recoveryKeyCreate,
  getConnectionSettings,
  type WorkspaceInfoDto,
} from "@/lib/ipc";
import { RelayPicker } from "@/components/RelayPicker";
import { relayUrlFor, type RelayChoice } from "@/lib/communityRelay";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { Modal } from "@/components/ui";

type Tab = "create" | "join";

/// Modal for the rail's ＋ button: create a team workspace (generates a room +
/// E2EE key) or join one from an invite code, plus manage/copy-invite/leave for
/// the workspaces already joined. Replaces the old "dump into Settings" flow.
export function AddWorkspaceModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const qc = useQueryClient();
  const [tab, setTab] = useState<Tab>("create");
  const [name, setName] = useState("");
  const [invite, setInvite] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [lastInvite, setLastInvite] = useState<string | null>(null);
  // A new workspace is secured before anything else: the founder's passkey
  // and recovery key (rules v2 from the first event; nobody can be let in,
  // and nothing governed, until the founder's passkey exists).
  const [securing, setSecuring] = useState<WorkspaceInfoDto | null>(null);
  const [hasPasskey, setHasPasskey] = useState(false);
  const [words, setWords] = useState<string | null>(null);

  const workspaces = useQuery({
    queryKey: ["workspaces"],
    queryFn: listWorkspaces,
    enabled: open,
  });
  // With no relay configured, the creator picks one here (the community relay
  // preselected); with one, the new workspace uses it. Joiners take the relay
  // from the invite.
  const conn = useQuery({ queryKey: ["connection-settings"], queryFn: getConnectionSettings, enabled: open });
  const currentRelay = conn.data?.relayUrl?.trim() ?? "";
  const needsRelayPick = conn.isSuccess && !currentRelay;
  const [relayChoice, setRelayChoice] = useState<RelayChoice>("community");
  const [ownRelayUrl, setOwnRelayUrl] = useState("");
  const pickedRelayMissing = needsRelayPick && relayChoice === "own" && !ownRelayUrl.trim();
  const rooms = (workspaces.data ?? []).filter((w) => w.kind === "room");

  if (!open) return null;

  async function refresh() {
    await qc.invalidateQueries({ queryKey: ["workspaces"] });
    await qc.invalidateQueries({ queryKey: ["chats"] });
    // Channels are workspace-scoped too — refresh them or the new workspace can
    // briefly show the previous one's channels until another refetch lands.
    await qc.invalidateQueries({ queryKey: ["channels"] });
    await qc.invalidateQueries({ queryKey: ["sync-status"] });
    await qc.invalidateQueries({ queryKey: ["connection-settings"] });
  }

  async function onCreate() {
    const n = name.trim();
    if (!n || busy || pickedRelayMissing) return;
    setBusy(true);
    try {
      const ws = needsRelayPick
        ? await createWorkspace(n, relayUrlFor(relayChoice, ownRelayUrl))
        : await createWorkspace(n);
      setName("");
      setHasPasskey(false);
      setWords(null);
      setSecuring(ws);
      await refresh();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function onAddPasskey() {
    if (busy) return;
    setBusy(true);
    try {
      await passkeyRegister("Passkey");
      setHasPasskey(true);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function onRecoveryKey() {
    if (busy) return;
    setBusy(true);
    try {
      setWords(await recoveryKeyCreate());
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  /// Secured: show the invite (it needs a relay to be worth sharing).
  async function onSecured() {
    const ws = securing;
    setSecuring(null);
    setWords(null);
    if (!ws) return;
    try {
      setLastInvite(await workspaceInvite(ws.id));
      toast.success(`“${ws.name}” is ready. Share the invite below to add people.`);
    } catch (e) {
      const m = inviteError(e);
      setLastInvite(null);
      toast.info(`“${ws.name}” is ready. ${m.message}`, m.action);
    }
    await qc.invalidateQueries({ queryKey: ["security-status"] });
  }

  async function onJoin() {
    const code = invite.trim();
    if (!code || busy) return;
    setBusy(true);
    try {
      const ws = await joinWorkspace(code);
      setInvite("");
      await refresh();
      toast.success(`Joined “${ws.name}”.`);
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
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

  async function joinByShortCode() {
    const c = joinCode.trim();
    if (!c || busy) return;
    setBusy(true);
    try {
      const r = await redeemShortCode(c);
      setJoinCode("");
      await refresh();
      toast.success(r.kind === "workspace" ? `Joined “${r.label}”.` : "Added.");
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  function leave(ws: WorkspaceInfoDto) {
    confirmThen(`Leave “${ws.name}”? Its chats stop syncing on this device.`, async () => {
      try {
        await removeWorkspace(ws.id);
        await refresh();
        toast.success(`Left “${ws.name}”.`);
      } catch (e) {
        toast.error(errMsg(e));
      }
    });
  }

  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-center justify-center p-4"
      overlayStyle={{ background: "color-mix(in srgb, var(--ligus-ink) 45%, transparent)" }}
      panelClassName="w-full max-w-md rounded-2xl border p-5 shadow-2xl"
      panelStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
    >
      <div className="mb-3 flex items-center justify-between">
        <h2 className="ligus-title">Add workspace</h2>
        <button className="text-ink-soft hover:text-ink" aria-label="Close" onClick={onClose}>
          ✕
        </button>
      </div>

      <div className="mb-4 flex gap-1 rounded-lg p-1" style={{ background: "color-mix(in srgb, var(--ligus-ink) 12%, transparent)" }}>
        {(["create", "join"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className="flex-1 rounded-md px-3 py-1.5 text-sm font-medium capitalize transition-colors"
            style={{
              background: tab === t ? "var(--ligus-accent-fill)" : "transparent",
              color: tab === t ? "var(--ligus-on-accent)" : "var(--ligus-ink)",
            }}
          >
            {t === "create" ? "Create" : "Join"}
          </button>
        ))}
      </div>

      {securing ? (
        <div className="space-y-3" data-testid="secure-workspace">
          <p className="text-sm font-semibold">Secure “{securing.name}”</p>
          <p className="text-xs text-ink-soft">
            Every change to who's in this workspace, their roles, devices and keys needs your passkey. Create one
            now, then write down a recovery key: it stands in for a lost passkey, and without it a sole owner who
            loses their passkeys can't govern the workspace again.
          </p>
          <button
            onClick={onAddPasskey}
            disabled={busy || hasPasskey}
            className="w-full rounded-lg px-3 py-2 text-sm font-semibold disabled:opacity-40"
            style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
          >
            {hasPasskey ? "Passkey created ✓" : busy ? "Waiting for your passkey…" : "1. Create a passkey"}
          </button>
          {words ? (
            <div className="space-y-2 rounded-lg border p-2" style={{ borderColor: "var(--ligus-line)" }}>
              <code className="block whitespace-pre-wrap text-xs" data-testid="new-workspace-recovery-words">
                {words}
              </code>
              <button
                onClick={onSecured}
                className="w-full rounded-lg px-3 py-2 text-sm font-semibold"
                style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
              >
                I wrote them down
              </button>
            </div>
          ) : (
            <button
              onClick={onRecoveryKey}
              disabled={busy || !hasPasskey}
              className="w-full rounded-lg px-3 py-2 text-sm font-semibold disabled:opacity-40"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
            >
              2. Create a recovery key
            </button>
          )}
        </div>
      ) : tab === "create" ? (
        <div className="space-y-3">
          <p className="text-xs text-ink-soft">
            Creates a private, end-to-end-encrypted team room. You become the owner and get an
            invite code to share — anyone who pastes it joins and syncs with you.
          </p>
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && onCreate()}
            placeholder="Workspace name (e.g. Acme App)"
            className="w-full rounded-lg border px-3 py-2 text-sm outline-none"
            style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}
          />
          {needsRelayPick ? (
            <div className="space-y-1.5" data-testid="new-workspace-relay">
              <div className="text-xs font-medium">Sync through</div>
              <RelayPicker
                choice={relayChoice}
                onChoice={setRelayChoice}
                ownUrl={ownRelayUrl}
                onOwnUrl={setOwnRelayUrl}
                allowNone
                inputClassName="w-full rounded-lg border px-3 py-2 font-mono text-xs outline-none"
                inputStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}
              />
            </div>
          ) : (
            currentRelay && <p className="text-xs text-ink-soft">Syncs through {currentRelay}.</p>
          )}
          <button
            onClick={onCreate}
            disabled={busy || !name.trim() || pickedRelayMissing}
            className="w-full rounded-lg px-3 py-2 text-sm font-semibold disabled:opacity-40"
            style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
          >
            {busy ? "Creating…" : "Create workspace"}
          </button>
          {lastInvite && (
            <div className="rounded-lg border p-2" style={{ borderColor: "var(--ligus-line)" }}>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-ink-soft">
                Invite code
              </div>
              <code className="block break-all text-xs text-ink-soft">{lastInvite}</code>
              <button
                className="mt-2 rounded-md px-2 py-1 text-xs font-medium underline text-ink-soft hover:text-ink"
                onClick={() => {
                  navigator.clipboard.writeText(lastInvite);
                  toast.success("Invite copied.");
                }}
              >
                Copy invite
              </button>
            </div>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          <p className="text-xs text-ink-soft">
            Paste an invite code (<code>hivews1:…</code>) someone shared with you. It carries the
            relay, room, and key needed to sync.
          </p>
          <textarea
            autoFocus
            value={invite}
            onChange={(e) => setInvite(e.target.value)}
            placeholder="hivews1:…"
            rows={3}
            className="w-full resize-none rounded-lg border px-3 py-2 font-mono text-xs outline-none"
            style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}
          />
          <button
            onClick={onJoin}
            disabled={busy || !invite.trim()}
            className="w-full rounded-lg px-3 py-2 text-sm font-semibold disabled:opacity-40"
            style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
          >
            {busy ? "Joining…" : "Join workspace"}
          </button>
          <div className="flex items-center gap-2 pt-1 text-xs text-ink-soft">
            <div className="h-px flex-1" style={{ background: "var(--ligus-line)" }} />
            or a short code
            <div className="h-px flex-1" style={{ background: "var(--ligus-line)" }} />
          </div>
          <div className="flex gap-2">
            <input
              value={joinCode}
              onChange={(e) => setJoinCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && joinByShortCode()}
              placeholder="e.g. K7P2QX"
              className="flex-1 rounded-lg border px-3 py-2 font-mono text-sm uppercase outline-none"
              style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}
            />
            <button
              onClick={joinByShortCode}
              disabled={busy || !joinCode.trim()}
              className="shrink-0 rounded-lg px-3 py-2 text-sm font-semibold disabled:opacity-40"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
            >
              Use code
            </button>
          </div>
        </div>
      )}

      {rooms.length > 0 && (
        <div className="mt-5 border-t pt-3" style={{ borderColor: "var(--ligus-line)" }}>
          <div className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-ink-soft">
            Your team workspaces
          </div>
          <ul className="space-y-1">
            {rooms.map((w) => (
              <li key={w.id} className="flex items-center justify-between gap-2 text-sm">
                <span className="truncate">{w.name}</span>
                <span className="flex shrink-0 gap-2 text-xs">
                  <button className="underline text-ink-soft hover:text-ink" onClick={() => shareShortCode(w)}>
                    Short code
                  </button>
                  <button className="underline text-ink-soft hover:text-ink" onClick={() => copyInvite(w)}>
                    Copy invite
                  </button>
                  <button
                    className="underline"
                    style={{ color: "var(--ligus-danger-ink)" }}
                    onClick={() => leave(w)}
                  >
                    Leave
                  </button>
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Modal>
  );
}
