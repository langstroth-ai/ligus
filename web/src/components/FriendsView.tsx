import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listCoMembers,
  listDms,
  listPendingInvites,
  listWorkspaces,
  myProfile,
  openDm,
  coMemberInvite,
  inspectInvite,
  joinDm,
  joinWorkspace,
  setActiveWorkspace,
  type CoMemberDto,
  type InviteInfoDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { Avatar } from "@/components/Avatar";
import { IconUsers } from "@/lib/icons";
import { confirmSafetyCode, trustNote } from "@/lib/people";

/// What the contacts status means for delivery. Exported for unit testing.
export function contactsNote(status: string | undefined): string | null {
  switch (status) {
    case "unsupported":
      return "Your relay doesn't support contacts yet, so invites can't be delivered in the app. Share invite links or QR codes instead.";
    case "noRelay":
      return "No relay is configured: invites are links you hand over yourself.";
    default:
      return null;
  }
}

/// A link to hand over, shown when an invite couldn't be delivered in the app.
function LinkBox({ title, link, code, onClose }: { title: string; link: string; code: string | null; onClose: () => void }) {
  return (
    <div className="rounded-2xl border p-4 text-sm" style={{ borderColor: "var(--hive-accent-cool)", background: "var(--hive-panel)" }}>
      <div className="font-semibold">{title}</div>
      <p className="mt-1 text-xs text-ink-soft">
        Send this link over a channel you trust. It works once. They will see your safety code; if they ask, read it to
        them{code ? <>. Theirs is <code className="font-mono">{code}</code></> : null}.
      </p>
      <textarea readOnly value={link} rows={3} className="mt-2 w-full rounded-lg border px-2 py-1 font-mono text-[11px]" style={{ borderColor: "var(--hive-line)" }} />
      <div className="mt-2 flex gap-2">
        <button
          className="rounded-lg px-3 py-1.5 text-xs font-medium"
          style={{ background: "var(--hive-accent-fill)", color: "var(--hive-on-accent)" }}
          onClick={() => {
            void navigator.clipboard.writeText(link);
            toast.success("Invite link copied.");
          }}
        >
          Copy link
        </button>
        <button className="text-xs opacity-60 hover:opacity-100" onClick={onClose}>
          Done
        </button>
      </div>
    </div>
  );
}

/// People: everyone you share a workspace with (keyed by account and safety
/// code, never by a handle), your direct messages, and joining from an invite
/// link. There is no directory and no friend requests: you reach people
/// through workspaces you share, or by handing them a link.
export function FriendsView({
  onOpenDm,
}: {
  onOpenDm?: () => void;
  onOpenSettings?: (tab: "Account" | "Team") => void;
} = {}) {
  const qc = useQueryClient();
  const profile = useQuery({ queryKey: ["my-profile"], queryFn: myProfile });
  const people = useQuery({ queryKey: ["co-members"], queryFn: listCoMembers, refetchInterval: 30_000 });
  const dms = useQuery({ queryKey: ["dms"], queryFn: listDms, refetchInterval: 30_000 });
  const workspaces = useQuery({ queryKey: ["workspaces"], queryFn: listWorkspaces });
  const pending = useQuery({ queryKey: ["pending-invites"], queryFn: listPendingInvites, refetchInterval: 20_000 });
  const [link, setLink] = useState<{ title: string; link: string; code: string | null } | null>(null);
  const [paste, setPaste] = useState("");
  const [info, setInfo] = useState<InviteInfoDto | null>(null);

  async function afterJoin() {
    await qc.invalidateQueries({ queryKey: ["workspaces"] });
    await qc.invalidateQueries({ queryKey: ["dms"] });
    await qc.invalidateQueries({ queryKey: ["chats"] });
  }

  async function message(p: CoMemberDto) {
    try {
      const r = await openDm(p.accountId);
      await afterJoin();
      if (r.invite && !r.delivered) {
        setLink({ title: `Your DM with ${p.displayName}`, link: r.invite, code: r.safetyCode });
      } else {
        if (r.delivered) toast.success(`${p.displayName} got your invite.`);
        onOpenDm?.();
      }
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function invite(p: CoMemberDto, workspaceId: string) {
    try {
      const r = await coMemberInvite(workspaceId, p.accountId);
      if (r.delivered) toast.success(`Invite delivered to ${r.name}.`);
      else setLink({ title: `Invite for ${r.name}`, link: r.invite, code: r.safetyCode });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function inspect(code: string) {
    setPaste(code);
    setInfo(null);
    if (!code.trim().startsWith("hivews1:")) return;
    try {
      setInfo(await inspectInvite(code.trim()));
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function join() {
    if (!info) return;
    try {
      if (info.isDm) {
        let confirmed = false;
        if (info.trust === "unknown") {
          confirmed = await confirmSafetyCode(info.safetyCode);
          if (!confirmed) return;
        }
        await joinDm(paste.trim(), confirmed);
      } else {
        await joinWorkspace(paste.trim());
      }
      setPaste("");
      setInfo(null);
      await afterJoin();
      toast.success("Joined.");
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function openExistingDm(workspaceId: string) {
    try {
      await setActiveWorkspace(workspaceId);
      await afterJoin();
      onOpenDm?.();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  const rooms = (workspaces.data ?? []).filter((w) => w.kind === "room");
  const note = contactsNote(pending.data?.contacts);

  return (
    <div className="mx-auto flex max-w-xl flex-col gap-6 p-8" style={{ color: "var(--hive-ink)" }}>
      <header className="flex items-center justify-between gap-3">
        <h1 className="flex items-center gap-2 text-lg font-semibold">
          <IconUsers size={18} /> People
        </h1>
        {profile.data?.safetyCode && (
          <span className="text-right text-[11px] text-ink-soft" title="Your safety code: people compare it with the one they see for you">
            Your code <code className="font-mono">{profile.data.safetyCode}</code>
          </span>
        )}
      </header>
      {note && <p className="text-xs text-ink-soft">{note}</p>}
      {link && (
        <LinkBox
          {...link}
          onClose={() => {
            setLink(null);
            onOpenDm?.();
          }}
        />
      )}

      <section className="flex flex-col gap-2">
        <h2 className="text-[11px] font-semibold uppercase tracking-wider text-ink-soft">Join with an invite</h2>
        <textarea
          value={paste}
          onChange={(e) => void inspect(e.target.value)}
          rows={2}
          placeholder="Paste an invite link: hivews1:…"
          className="w-full rounded-lg border px-3 py-2 font-mono text-xs"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
          aria-label="Invite link"
        />
        {info && (
          <div className="rounded-lg border px-3 py-2 text-xs" style={{ borderColor: info.trust === "mismatch" ? "var(--hive-danger-ink)" : "var(--hive-line)" }}>
            <div className="text-sm font-medium">{info.isDm ? "Direct message" : info.workspaceName}</div>
            {info.isDm && !info.addressedToMe ? (
              <div style={{ color: "var(--hive-danger-ink)" }}>This DM invite is addressed to someone else.</div>
            ) : (
              <div style={{ color: info.trust === "mismatch" ? "var(--hive-danger-ink)" : undefined }}>{trustNote(info.trust, info.founderName)}</div>
            )}
            {info.safetyCode && <code className="mt-1 block font-mono text-[11px] text-ink-soft">{info.safetyCode}</code>}
            <button
              className="mt-2 rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-40"
              style={{ background: "var(--hive-accent-fill)", color: "var(--hive-on-accent)" }}
              disabled={info.alreadyJoined || info.trust === "mismatch" || (info.isDm && !info.addressedToMe)}
              onClick={() => void join()}
            >
              {info.alreadyJoined ? "Already joined" : "Join"}
            </button>
          </div>
        )}
      </section>

      {(dms.data?.length ?? 0) > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="text-[11px] font-semibold uppercase tracking-wider text-ink-soft">Direct messages</h2>
          {dms.data!.map((d) => (
            <button
              key={d.workspaceId}
              onClick={() => void openExistingDm(d.workspaceId)}
              className="flex items-center justify-between rounded-lg border px-3 py-2 text-left"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
            >
              <span className="flex items-center gap-2 text-sm">
                <Avatar name={d.name} url={d.avatarUrl} kind="human" size={22} />
                {d.name}
              </span>
              {d.archived.length > 0 && (
                <span className="text-[11px] text-ink-soft" title="You both opened a DM at the same time; the older one was kept">
                  +{d.archived.length} archived
                </span>
              )}
            </button>
          ))}
        </section>
      )}

      <section className="flex flex-col gap-2">
        <h2 className="text-[11px] font-semibold uppercase tracking-wider text-ink-soft">People you share a workspace with</h2>
        {people.data && people.data.length === 0 && (
          <p className="text-sm text-ink-soft">Nobody yet. Create a workspace and share its invite link, or join one from a link above.</p>
        )}
        {people.data?.map((p) => {
          const elsewhere = rooms.filter((w) => !p.workspaces.some((x) => x.id === w.id));
          return (
            <div
              key={p.accountId}
              className="flex flex-col gap-1.5 rounded-lg border px-3 py-2"
              style={{ borderColor: p.keyConflict ? "var(--hive-danger-ink)" : "var(--hive-line)", background: "var(--hive-panel)" }}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="flex min-w-0 items-center gap-2 text-sm">
                  <Avatar name={p.displayName} url={p.avatarUrl} kind="human" size={24} />
                  <span className="truncate font-medium">{p.displayName || "Unnamed"}</span>
                </span>
                <button
                  onClick={() => (p.dmWorkspaceId ? void openExistingDm(p.dmWorkspaceId) : void message(p))}
                  disabled={p.keyConflict}
                  className="shrink-0 rounded-md border px-3 py-1 text-xs font-medium hover:brightness-110 disabled:opacity-40"
                  style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)" }}
                >
                  Message
                </button>
              </div>
              <div className="font-mono text-[11px] text-ink-soft" title="Their safety code">
                {p.keyConflict
                  ? "Your shared workspaces disagree about this person's key: nothing is sent to them."
                  : (p.safetyCode ?? "no safety code yet")}
              </div>
              <div className="flex flex-wrap items-center gap-1 text-[11px] text-ink-soft">
                In {p.workspaces.map((w) => w.name).join(", ") || "a direct message"}
                {elsewhere.length > 0 && !p.keyConflict && (
                  <select
                    className="ml-auto rounded border px-1 py-0.5 text-[11px]"
                    style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)" }}
                    value=""
                    aria-label={`Invite ${p.displayName} to a workspace`}
                    onChange={(e) => e.target.value && void invite(p, e.target.value)}
                  >
                    <option value="">Invite to…</option>
                    {elsewhere.map((w) => (
                      <option key={w.id} value={w.id}>
                        {w.name}
                      </option>
                    ))}
                  </select>
                )}
              </div>
            </div>
          );
        })}
      </section>
    </div>
  );
}
