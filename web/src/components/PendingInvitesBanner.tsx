import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listPendingInvites, acceptPendingInvite, dismissPendingInvite, type PendingInvite } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { Button } from "@/components/ui";
import { InboxItem } from "@/components/kit";
import { confirmSafetyCode, trustNote } from "@/lib/people";

/// Invites delivered to this device's contact drop box (sealed to it, opened
/// locally), as Inbox rows. Each shows who founded the workspace or DM, by
/// safety code: a name proves nothing. Accept joins; Dismiss deletes it from
/// the relay at the next read. Nothing renders when there are none,
/// including on a relay without contacts (invites then come as links).
export function PendingInviteRows() {
  const qc = useQueryClient();
  const invites = useQuery({
    queryKey: ["pending-invites"],
    queryFn: listPendingInvites,
    refetchInterval: 20000,
  });
  const accept = useMutation({
    mutationFn: async (inv: PendingInvite) => {
      let confirmed = false;
      if (inv.isDm && inv.trust === "unknown") {
        confirmed = await confirmSafetyCode(inv.safetyCode);
        if (!confirmed) return null;
      }
      return acceptPendingInvite(inv, confirmed);
    },
    onSuccess: async (ws) => {
      if (!ws) return;
      toast.success(`Joined ${ws.name}.`);
      await qc.invalidateQueries({ queryKey: ["workspaces"] });
      await qc.invalidateQueries({ queryKey: ["dms"] });
      await qc.invalidateQueries({ queryKey: ["pending-invites"] });
    },
    onError: (e) => toast.error(`Couldn't join: ${errMsg(e)}`),
  });
  const dismiss = useMutation({
    mutationFn: (inv: PendingInvite) => dismissPendingInvite(inv),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["pending-invites"] }),
    onError: (e) => toast.error(`Couldn't dismiss: ${errMsg(e)}`),
  });

  const list = invites.data?.invites ?? [];
  if (list.length === 0) return null;

  return (
    <>
      {list.map((inv) => {
        const key = `${inv.relay}#${inv.seq}`;
        const joining = accept.isPending && accept.variables?.seq === inv.seq && accept.variables?.relay === inv.relay;
        const dismissing = dismiss.isPending && dismiss.variables?.seq === inv.seq && dismiss.variables?.relay === inv.relay;
        const busy = joining || dismissing;
        const bad = inv.trust === "mismatch";
        // Only a signed invite from a co-member is "from" them; anything else
        // just says it is. An unsigned DM invite can't be accepted.
        const proven = inv.trust === "known";
        const refused = bad || (inv.isDm && inv.trust === "claimed");
        const who = inv.founderName || (inv.isDm ? "Someone" : "");
        const title = inv.isDm
          ? proven
            ? `${who} wants to message you`
            : `A message invite that says it's from ${inv.founderName || "someone"}`
          : `Join ${inv.workspaceName}${inv.founderName ? (proven ? ` (founded by ${inv.founderName})` : ` (says it's from ${inv.founderName})`) : ""}`;
        return (
          <InboxItem
            key={key}
            state={bad ? "failed" : "idle"}
            title={title}
            context={inv.isDm ? "Direct message" : "Workspace invite"}
            detail={
              <>
                <span style={{ color: bad ? "var(--ligus-danger-ink)" : undefined }}>{trustNote(inv.trust, inv.founderName)}</span>
                {inv.safetyCode && (
                  <code className="mt-0.5 block font-mono text-[11px]" data-tip="The founder's safety code">
                    {inv.safetyCode}
                  </code>
                )}
              </>
            }
            actions={
              <>
                {!refused && (
                  <Button size="sm" variant="primary" onClick={() => accept.mutate(inv)} disabled={busy}>
                    {joining ? "Joining…" : "Accept"}
                  </Button>
                )}
                <Button size="sm" variant="quiet" onClick={() => dismiss.mutate(inv)} disabled={busy}>
                  Dismiss
                </Button>
              </>
            }
          />
        );
      })}
    </>
  );
}
