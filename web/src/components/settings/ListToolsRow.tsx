import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getListTools, setListTools } from "@/lib/ipc";
import { InfoTip, Switch } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// "List this computer's tools": whether this computer's host record names
/// the coding tools it can run (Claude Code, Codex, …) and their sign-in
/// state, so members can place agents here. Names only; never a key.
export function ListToolsRow() {
  const qc = useQueryClient();
  const on = useQuery({ queryKey: ["list-tools"], queryFn: getListTools, retry: false });
  const save = useMutation({
    mutationFn: (v: boolean) => setListTools(v),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["list-tools"] });
      qc.invalidateQueries({ queryKey: ["workspace-hosts"] });
    },
    onError: (e) => toast.error(`Couldn't save: ${errMsg(e)}`),
  });
  if (on.data === undefined) return null;
  return (
    <div className="mb-3 flex items-center justify-between gap-2 border-b pb-3" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="flex items-center gap-1.5">
        <span className="text-sm">List this computer's tools</span>
        <InfoTip
          label="List this computer's tools"
          text="Members see which coding tools this computer can run and whether they're signed in, so they can place agents here. Names only: no keys, accounts or addresses. Off: this computer lists none."
        />
      </div>
      <Switch on={on.data} label="List this computer's tools" disabled={save.isPending} onChange={(v) => save.mutate(v)} />
    </div>
  );
}
