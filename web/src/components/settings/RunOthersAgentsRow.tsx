import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getRunOthersAgents, setRunOthersAgents } from "@/lib/ipc";
import { InfoTip, Switch } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// "Run other people's agents on this computer": whether an agent another
/// member placed on this computer runs here, on this computer's tools and
/// sign-ins. Off by default: this computer runs only your own agents, and
/// the others' chats say it won't (workspace model v3, D16).
export function RunOthersAgentsRow() {
  const qc = useQueryClient();
  const on = useQuery({ queryKey: ["run-others-agents"], queryFn: getRunOthersAgents, retry: false });
  const save = useMutation({
    mutationFn: (v: boolean) => setRunOthersAgents(v),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["run-others-agents"] }),
    onError: (e) => toast.error(`Couldn't save: ${errMsg(e)}`),
  });
  if (on.data === undefined) return null;
  return (
    <div className="mb-3 flex items-center justify-between gap-2 border-b pb-3" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="flex items-center gap-1.5">
        <span className="text-sm">Run other people's agents on this computer</span>
        <InfoTip
          label="Run other people's agents on this computer"
          text="Off: this computer runs only your own agents. An agent someone else placed here doesn't run, and their chat says so. On: it runs on this computer's tools and sign-ins."
        />
      </div>
      <Switch on={on.data} label="Run other people's agents on this computer" disabled={save.isPending} onChange={(v) => save.mutate(v)} />
    </div>
  );
}
