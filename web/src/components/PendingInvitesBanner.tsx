import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listPendingInvites, acceptPendingInvite, dismissPendingInvite, type PendingInvite } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { IconUsers, IconX } from "@/lib/icons";
import { confirmSafetyCode, trustNote } from "@/lib/people";

/// A quiet top-right banner for invites delivered to this device's contact
/// drop box (sealed to it, opened locally). Each shows who founded the
/// workspace or DM, by safety code: a name proves nothing. Accept joins;
/// Dismiss deletes it from the relay at the next read. Silent when there are
/// none, including on a relay without contacts (invites then come as links).
export function PendingInvitesBanner() {
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
    // Cap the stack to the viewport with internal scroll.
    <div className="fixed right-4 top-4 z-[60] flex max-h-[calc(100vh-2rem)] w-[320px] flex-col gap-2 overflow-y-auto">
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
        return (
          <div
            key={key}
            className="rounded-2xl border p-4 shadow-xl"
            style={{ borderColor: bad ? "var(--ligus-danger-ink)" : "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
          >
            <div className="flex items-start gap-3">
              <div className="mt-0.5" style={{ color: "var(--ligus-accent-ink)" }} aria-hidden>
                <IconUsers size={18} />
              </div>
              <div className="min-w-0 flex-1">
                <div className="text-sm font-semibold">{inv.isDm ? "Direct message" : "Workspace invite"}</div>
                <div className="mt-0.5 text-sm text-ink-soft">
                  {inv.isDm ? (
                    <>
                      {proven ? (
                        <>
                          <b>{inv.founderName || "Someone"}</b> wants to message you
                        </>
                      ) : (
                        <>
                          A message invite that says it's from <b>{inv.founderName || "someone"}</b>
                        </>
                      )}
                    </>
                  ) : (
                    <>
                      Join <b>{inv.workspaceName}</b>
                      {inv.founderName ? (
                        proven ? <> (founded by {inv.founderName})</> : <> (says it's from {inv.founderName})</>
                      ) : null}
                    </>
                  )}
                </div>
                <div className="mt-1 text-xs" style={{ color: bad ? "var(--ligus-danger-ink)" : undefined }}>
                  {trustNote(inv.trust, inv.founderName)}
                </div>
                {inv.safetyCode && (
                  <code className="mt-1 block font-mono text-[11px] text-ink-soft" title="The founder's safety code">
                    {inv.safetyCode}
                  </code>
                )}
                <div className="mt-3 flex items-center gap-2">
                  {!refused && (
                    <button
                      onClick={() => accept.mutate(inv)}
                      disabled={busy}
                      className="rounded-lg px-3 py-1.5 text-sm font-medium hover:brightness-110 disabled:opacity-70"
                      style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
                    >
                      {joining ? "Joining…" : "Accept"}
                    </button>
                  )}
                  <button
                    onClick={() => dismiss.mutate(inv)}
                    disabled={busy}
                    className="rounded-lg px-3 py-1.5 text-sm text-ink-soft hover:text-ink disabled:opacity-40"
                  >
                    Dismiss
                  </button>
                </div>
              </div>
              <button
                onClick={() => dismiss.mutate(inv)}
                disabled={busy}
                aria-label="Dismiss"
                className="shrink-0 text-ink-soft hover:text-ink disabled:opacity-20"
              >
                <IconX size={14} />
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}
