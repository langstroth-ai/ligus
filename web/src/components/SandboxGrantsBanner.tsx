import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ackSandboxGrants, getSandboxPolicy } from "@/lib/ipc";
import { openSettingsTab } from "@/lib/nav";
import { toast, errMsg } from "@/components/Toast";
import { Button } from "@/components/ui";
import { InboxItem } from "@/components/kit";

/// The workspace's signed sandbox grants changed since you last looked: a
/// grant opens paths (or the shell) to agent turns on every device that
/// applies grants, so the change waits in the Inbox until you review or
/// dismiss it. Says whether this device applies them.
export function SandboxGrantsNotice() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["sandbox-policy", null], queryFn: () => getSandboxPolicy(null), refetchInterval: 30000 });
  const ack = useMutation({
    mutationFn: ackSandboxGrants,
    onSuccess: () => qc.invalidateQueries({ queryKey: ["sandbox-policy"] }),
    onError: (e) => toast.error(`Couldn't dismiss: ${errMsg(e)}`),
  });
  const d = q.data;
  if (!d || !d.changed) return null;
  const empty = !d.grants.workspace && !d.grants.agents;
  const review = () => {
    openSettingsTab("sandbox");
    ack.mutate();
  };
  return (
    <InboxItem
      state="idle"
      title={empty ? "This workspace's sandbox grants were withdrawn" : "This workspace's sandbox grants changed"}
      detail={d.appliesHere ? "They apply to agent turns on this device." : "This device doesn't apply them."}
      onOpen={review}
      actions={
        <>
          <Button size="sm" onClick={review}>
            Review
          </Button>
          <Button size="sm" variant="quiet" onClick={() => ack.mutate()} disabled={ack.isPending}>
            Dismiss
          </Button>
        </>
      }
    />
  );
}
