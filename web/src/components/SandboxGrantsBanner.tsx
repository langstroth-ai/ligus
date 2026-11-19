import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ackSandboxGrants, getSandboxPolicy } from "@/lib/ipc";
import { openSettingsTab } from "@/lib/nav";
import { toast, errMsg } from "@/components/Toast";
import { IconShield } from "@/lib/icons";

/// The workspace's signed sandbox grants changed since you last looked: a
/// grant opens paths (or the shell) to agent turns on every device that
/// applies grants, so the change is shown until you review or dismiss it.
/// Says whether this device applies them.
export function SandboxGrantsBanner() {
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
  return (
    <div
      role="status"
      className="fixed bottom-4 left-4 z-[60] flex w-[360px] items-start gap-3 rounded-2xl border p-4 shadow-xl"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <div className="mt-0.5" style={{ color: "var(--hive-accent-ink)" }} aria-hidden>
        <IconShield size={18} />
      </div>
      <div className="min-w-0 flex-1 text-sm">
        {empty ? "This workspace's sandbox grants were withdrawn." : "This workspace's sandbox grants changed."}{" "}
        {d.appliesHere ? "They apply to agent turns on this device." : "This device doesn't apply them."}
        <div className="mt-2 flex gap-2">
          <button
            onClick={() => {
              openSettingsTab("sandbox");
              ack.mutate();
            }}
            className="rounded-lg px-3 py-1.5 text-sm font-medium hover:brightness-110"
            style={{ background: "var(--hive-accent-fill)", color: "var(--hive-on-accent)" }}
          >
            Review
          </button>
          <button onClick={() => ack.mutate()} className="rounded-lg px-3 py-1.5 text-sm opacity-80 hover:opacity-100">
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}
