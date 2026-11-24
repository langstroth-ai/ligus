import { useEffect, useState } from "react";
import { useRowMenu } from "@/components/RowMenu";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  identityLinkStatuses,
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
  heldDmInvite,
  confirmDmInvite,
  dismissDmInvite,
  onDmInviteChanged,
  type CoMemberDto,
  type DmDto,
  type InviteDelivery,
  type InviteInfoDto,
} from "@/lib/ipc";
import { confirmDialog } from "@/components/Dialog";
import { toast, errMsg } from "@/components/Toast";
import { Avatar } from "@/components/Avatar";
import { Button } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { IconUsers } from "@/lib/icons";
import { confirmSafetyCode, trustNote } from "@/lib/people";
import { LinkChips } from "@/components/LinkChips";

/// A co-member's linked accounts. Opening People checks them with their
/// forges (this device's 24-hour cache answers first, so it is at most one
/// request per link a day); until then the cached status shows.
/// A link whose server its owner chose (a self-run PDS) is only asked when
/// you click "Check now": an automatic check would tell that server who
/// looked.
function PersonLinks({ p }: { p: CoMemberDto }) {
  const qc = useQueryClient();
  const has = (p.links?.length ?? 0) > 0;
  const checked = useQuery({
    queryKey: ["links-of", p.accountId],
    queryFn: () => identityLinkStatuses(p.accountId, false),
    enabled: has,
    staleTime: 10 * 60_000,
  });
  if (!has) return null;
  const shown = checked.data ?? p.links;
  const unchecked = (shown ?? []).some((l) => l.status?.state === "notChecked");
  const checkNow = async () => {
    try {
      qc.setQueryData(["links-of", p.accountId], await identityLinkStatuses(p.accountId, true));
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-1">
      <LinkChips links={shown} />
      {unchecked && (
        <button type="button" className="text-[11px] underline text-ink-soft" onClick={() => void checkNow()}>
          Check now
        </button>
      )}
    </div>
  );
}

/// What the contacts status means for delivery. Exported for unit testing.
export function contactsNote(status: string | undefined): string | null {
  switch (status) {
    case "unsupported":
      return "Your relay can't deliver invites yet: share links or QR codes.";
    case "noRelay":
      return "No relay: invites are links you hand over.";
    default:
      return null;
  }
}

/// What a DM's held invite means, in words (null when nothing waits).
/// Exported for unit testing.
export function inviteDeliveryNote(d: InviteDelivery | null | undefined, name: string): string | null {
  if (!d) return null;
  switch (d.state) {
    case "waiting":
    case "sending":
      return `Your invite will be delivered when ${name}'s device is reachable. You can also send them the link yourself.`;
    case "warning":
      return `${name}'s keys changed since you opened this DM, so the invite wasn't sent. Compare safety codes, then deliver it to their current devices or send the link yourself.`;
    case "delivered":
      return `${name} got your invite.`;
    case "expired":
      return `Your invite wasn't delivered within 7 days. If you kept the link, send it to ${name}; otherwise ask them to open a DM with you.`;
    default:
      return null;
  }
}

/// A link to hand over, shown when an invite couldn't be delivered in the app.
function LinkBox({
  title,
  link,
  code,
  note,
  onClose,
}: {
  title: string;
  link: string;
  code: string | null;
  note?: string | null;
  onClose: () => void;
}) {
  return (
    <div className="rounded-2xl border p-4 text-sm" style={{ borderColor: "var(--ligus-accent-cool)", background: "var(--ligus-panel)" }}>
      <div className="font-semibold">{title}</div>
      {note && <p className="mt-1 text-xs">{note}</p>}
      <p className="mt-1 text-xs text-ink-soft">
        Send this link over a channel you trust. It works once. They will see your safety code; if they ask, read it to
        them{code ? <>. Theirs is <code className="font-mono">{code}</code></> : null}.
      </p>
      <textarea readOnly value={link} rows={3} className="mt-2 w-full rounded-lg border px-2 py-1 font-mono text-[11px]" style={{ borderColor: "var(--ligus-line)" }} />
      <div className="mt-2 flex gap-2">
        <button
          className="rounded-lg px-3 py-1.5 text-xs font-medium"
          style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
          onClick={() => {
            void navigator.clipboard.writeText(link);
            toast.success("Invite link copied.");
          }}
        >
          Copy link
        </button>
        <button className="text-xs text-ink-soft hover:text-ink" onClick={onClose}>
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
  const [link, setLink] = useState<{ title: string; link: string; code: string | null; note?: string | null } | null>(null);

  // A held DM invite was delivered (or expired, or needs a decision).
  useEffect(() => {
    let off: (() => void) | undefined;
    void onDmInviteChanged((e) => {
      void qc.invalidateQueries({ queryKey: ["dms"] });
      if (e.inviteDelivery.state === "delivered") {
        const name = dms.data?.find((d) => d.workspaceId === e.workspaceId)?.name ?? "They";
        toast.success(`${name} got your DM invite.`);
      }
    })
      .then((u) => (off = u))
      .catch(() => {});
    return () => off?.();
  }, [qc, dms.data]);
  const [paste, setPaste] = useState("");
  const [joining, setJoining] = useState(false);
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
        setLink({
          title: `Your DM with ${p.displayName}`,
          link: r.invite,
          code: r.safetyCode,
          note: r.pending ? inviteDeliveryNote(r.inviteDelivery, p.displayName) : null,
        });
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
      setJoining(false);
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

  async function shareHeld(d: DmDto) {
    try {
      const l = await heldDmInvite(d.workspaceId);
      setLink({ title: `Your DM with ${d.name}`, link: l, code: d.safetyCode, note: inviteDeliveryNote(d.inviteDelivery, d.name) });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function deliverAnyway(d: DmDto) {
    // Bound to the code shown here: the host refuses it if theirs changed
    // again before the click.
    const code = d.safetyCode;
    if (!code) {
      toast.error(`${d.name} has no safety code to compare yet; send the link yourself.`);
      return;
    }
    const ok = await confirmDialog(
      `${d.inviteDelivery?.warning ?? "Their keys changed."} Deliver the invite to ${d.name}'s current devices only if their safety code still matches what they tell you (${code}).`,
      { title: "Unverified recipient", confirmLabel: "Deliver anyway", danger: true },
    );
    if (!ok) return;
    try {
      await confirmDmInvite(d.workspaceId, code);
      await qc.invalidateQueries({ queryKey: ["dms"] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function stopWaiting(d: DmDto) {
    try {
      await dismissDmInvite(d.workspaceId);
      await qc.invalidateQueries({ queryKey: ["dms"] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  const rooms = (workspaces.data ?? []).filter((w) => w.kind === "room");
  const note = contactsNote(pending.data?.contacts);

  return (
    // Fill the canvas (and scroll inside it) so the status bar stays at the foot.
    <div className="min-h-0 w-full flex-1 overflow-y-auto">
    <div className="mx-auto flex max-w-xl flex-col gap-6 p-8" style={{ color: "var(--ligus-ink)" }}>
      <header className="flex items-center justify-between gap-3">
        <h1 className="ligus-title flex items-center gap-2">
          <IconUsers size={18} /> People
        </h1>
        <Button size="sm" onClick={() => setJoining(true)}>
          Join with an invite…
        </Button>
      </header>
      {profile.data?.safetyCode && (
        <p className="-mt-4 text-[11px] text-ink-soft" data-tip="Your safety code: people compare it with the one they see for you">
          Your code <code className="font-mono">{profile.data.safetyCode}</code>
        </p>
      )}
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

      {joining && (
      <Sheet
        title="Join with an invite"
        side="center"
        width={460}
        onClose={() => {
          setJoining(false);
          setPaste("");
          setInfo(null);
        }}
      >
      <section className="flex flex-col gap-2">
        <textarea
          autoFocus
          value={paste}
          onChange={(e) => void inspect(e.target.value)}
          rows={2}
          placeholder="Paste an invite link: hivews1:…"
          className="w-full rounded-lg border px-3 py-2 font-mono text-xs"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
          aria-label="Invite link"
        />
        {info && (
          <div className="rounded-lg border px-3 py-2 text-xs" style={{ borderColor: info.trust === "mismatch" ? "var(--ligus-danger-ink)" : "var(--ligus-line)" }}>
            <div className="text-sm font-medium">{info.isDm ? "Direct message" : info.workspaceName}</div>
            {info.isDm && !info.addressedToMe ? (
              <div style={{ color: "var(--ligus-danger-ink)" }}>This DM invite is addressed to someone else.</div>
            ) : (
              <div style={{ color: info.trust === "mismatch" ? "var(--ligus-danger-ink)" : undefined }}>{trustNote(info.trust, info.founderName)}</div>
            )}
            {info.safetyCode && <code className="mt-1 block font-mono text-[11px] text-ink-soft">{info.safetyCode}</code>}
            <button
              className="mt-2 rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-40"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
              disabled={info.alreadyJoined || info.trust === "mismatch" || (info.isDm && (!info.addressedToMe || info.trust === "claimed"))}
              onClick={() => void join()}
            >
              {info.alreadyJoined ? "Already joined" : "Join"}
            </button>
          </div>
        )}
      </section>
      </Sheet>
      )}

      {(dms.data?.length ?? 0) > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="ligus-cap">Direct messages</h2>
          <ul className="ligus-rowlist" aria-label="Direct messages">
            {dms.data!.map((d) => (
              <DmRow
                key={d.workspaceId}
                d={d}
                onOpen={() => void openExistingDm(d.workspaceId)}
                onShare={() => void shareHeld(d)}
                onDeliverAnyway={() => void deliverAnyway(d)}
                onStopWaiting={() => void stopWaiting(d)}
              />
            ))}
          </ul>
        </section>
      )}

      <section className="flex flex-col gap-2">
        <h2 className="ligus-cap">In your workspaces</h2>
        {people.data && people.data.length === 0 && (
          <p className="text-sm text-ink-soft">Nobody yet.</p>
        )}
        <ul className="ligus-rowlist" aria-label="People in your workspaces">
          {people.data?.map((p) => (
            <PersonRow
              key={p.accountId}
              p={p}
              elsewhere={rooms.filter((w) => !p.workspaces.some((x) => x.id === w.id))}
              onMessage={() => (p.dmWorkspaceId ? void openExistingDm(p.dmWorkspaceId) : void message(p))}
              onInvite={(id) => void invite(p, id)}
            />
          ))}
        </ul>
      </section>
    </div>
    </div>
  );
}

/// One person: avatar, name, verified links and Message on one line. The
/// safety code, shared workspaces and "Invite to…" open with the row. A key
/// conflict is never hidden: it shows on the row itself.
function PersonRow({
  p,
  elsewhere,
  onMessage,
  onInvite,
}: {
  p: CoMemberDto;
  elsewhere: { id: string; name: string }[];
  onMessage: () => void;
  onInvite: (workspaceId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const menu = useRowMenu(`Actions for ${p.displayName || "this person"}`, [
    {
      label: "Message",
      disabled: p.keyConflict,
      title: p.keyConflict ? "Nothing is sent while their key is in conflict" : undefined,
      onSelect: onMessage,
    },
    ...(p.keyConflict ? [] : elsewhere.map((w) => ({ label: `Invite to ${w.name}`, onSelect: () => onInvite(w.id) }))),
  ]);
  return (
    <li className="px-1" {...menu.rowProps}>
      <div className="ligus-listrow flex items-center justify-between gap-2">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="-mx-1 flex min-w-0 flex-1 items-center gap-2 rounded-lg px-1 text-left text-sm transition-colors hover:bg-[color:var(--ligus-overlay)]"
          data-tip={p.safetyCode ? `Safety code ${p.safetyCode}` : undefined}
        >
          <Avatar name={p.displayName} url={p.avatarUrl} kind="human" size={24} />
          <span className="truncate font-medium">{p.displayName || "Unnamed"}</span>
          {p.lookAlike && (
            <span
              className="shrink-0 rounded px-1.5 py-0.5 text-[11px] font-medium"
              style={{ color: "var(--ligus-danger-ink)", border: "1px solid var(--ligus-danger-ink)" }}
              data-tip="Another person here has a name that looks like this one. Compare safety codes to tell them apart."
            >
              Look-alike name
            </span>
          )}
        </button>
        <span className="flex min-w-0 shrink items-center">
          <PersonLinks p={p} />
        </span>
        {menu.button}
        {menu.menu}
      </div>
      {p.keyConflict && (
        <div className="pb-1.5 text-[11.5px]" role="alert" style={{ color: "var(--ligus-danger-ink)" }}>
          Your shared workspaces disagree about this person&apos;s key: nothing is sent to them.
        </div>
      )}
      {open && (
        <div className="flex flex-col gap-1 pb-2 pl-8">
          {!p.keyConflict && (
            <div className="font-mono text-[11px] text-ink-soft" data-tip="Their safety code">
              {p.safetyCode ?? "no safety code yet"}
            </div>
          )}
          <div className="text-[11px] text-ink-soft">
            In {p.workspaces.map((w) => w.name).join(", ") || "a direct message"}
          </div>
        </div>
      )}
    </li>
  );
}

/// One DM as a row: the click opens it; sharing the held invite, delivering
/// it anyway and giving up live in the row menu. A pending or unverified
/// delivery keeps its note visible under the row.
function DmRow({
  d,
  onOpen,
  onShare,
  onDeliverAnyway,
  onStopWaiting,
}: {
  d: DmDto;
  onOpen: () => void;
  onShare: () => void;
  onDeliverAnyway: () => void;
  onStopWaiting: () => void;
}) {
  const note = d.inviteDelivery?.pending || d.inviteDelivery?.state === "expired" ? inviteDeliveryNote(d.inviteDelivery, d.name) : null;
  const warn = d.inviteDelivery?.state === "warning";
  const pendingInvite = Boolean(d.inviteDelivery?.pending);
  const rm = useRowMenu(
    `Actions for the DM with ${d.name}`,
    pendingInvite
      ? [
          { label: "Share link", onSelect: onShare },
          ...(warn ? [{ label: "Deliver anyway…", danger: true, onSelect: onDeliverAnyway }] : []),
          { label: "Stop waiting", onSelect: onStopWaiting },
        ]
      : [],
  );
  return (
    <li className="px-1" {...rm.rowProps}>
      <div className="ligus-listrow flex items-center gap-2">
        <button
          type="button"
          onClick={onOpen}
          className="-mx-1 flex min-w-0 flex-1 items-center gap-2 rounded-lg px-1 text-left transition-colors hover:bg-[color:var(--ligus-overlay)]"
        >
          <Avatar name={d.name} url={d.avatarUrl} kind="human" size={22} />
          <span className="min-w-0 truncate text-[13px] font-medium">{d.name}</span>
          {d.archived.length > 0 && (
            <span className="shrink-0 text-[11px] text-ink-soft" data-tip="You both opened a DM at the same time; the older one was kept">
              +{d.archived.length} archived
            </span>
          )}
        </button>
        {rm.button}
      </div>
      {note && (
        <div className="pb-2 pl-[30px] text-[11px]" role={warn ? "alert" : undefined} style={{ color: warn ? "var(--ligus-danger-ink)" : "var(--ligus-ink-soft)" }}>
          {note}
        </div>
      )}
      {rm.menu}
    </li>
  );
}
