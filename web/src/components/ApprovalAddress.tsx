// A workspace's approval address (`ligusaddr1:`, design 5.1) with Copy:
// where a box or a device of yours sends its request so it reaches the
// Inbox. Not a secret and it grants nothing; an Admin can reset it.

import { useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getApprovalAddress, resetApprovalAddress } from "@/lib/ipc";
import { Button, InfoTip, LinkButton } from "@/components/ui";
import { confirmDialog } from "@/components/Dialog";
import { errMsg, toast } from "@/components/Toast";

export const ADDRESS_ABOUT =
  "Requests sent to this address reach the Inbox of the people who can approve them. It isn't a secret and grants nothing. If it reaches the wrong hands, an Admin can reset it.";

export function ApprovalAddress({ workspaceId, hint }: { workspaceId: string; hint: ReactNode }) {
  const qc = useQueryClient();
  const key = ["approval-address", workspaceId];
  const q = useQuery({ queryKey: key, queryFn: () => getApprovalAddress(workspaceId), retry: false, staleTime: 60_000 });
  const [busy, setBusy] = useState(false);

  async function copy() {
    if (!q.data) return;
    try {
      await navigator.clipboard.writeText(q.data.address);
      toast.success("Approval address copied.");
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function reset() {
    const ok = await confirmDialog("Reset the approval address? Requests sent to the old one stop arriving; scripts and manifests that hold it need the new one.", {
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      qc.setQueryData(key, await resetApprovalAddress(workspaceId));
      toast.success("A new approval address is ready.");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  if (q.isError) {
    return (
      <p className="text-[12.5px] text-ink-soft" data-testid="approval-address-none">
        {errMsg(q.error)}
      </p>
    );
  }
  return (
    <div className="space-y-1.5" data-testid="approval-address">
      <div className="flex items-center gap-1.5 text-[12.5px]">
        <span className="font-medium">Approval address</span>
        <InfoTip label="the approval address" text={ADDRESS_ABOUT} />
      </div>
      <div className="flex items-center gap-2">
        <code
          className="min-w-0 flex-1 truncate rounded-lg border px-2 py-1 text-[11.5px]"
          style={{ borderColor: "var(--ligus-line)", fontFamily: "var(--ligus-mono)" }}
          data-tip={q.data?.address}
        >
          {q.data?.address ?? "…"}
        </code>
        <Button size="sm" disabled={!q.data} onClick={() => void copy()}>
          Copy
        </Button>
      </div>
      <p className="text-[12px] text-ink-soft">
        {hint}
        {q.data?.canReset && (
          <>
            {" "}
            <LinkButton tone="ink" disabled={busy} onClick={() => void reset()}>
              Reset address
            </LinkButton>
          </>
        )}
      </p>
    </div>
  );
}
