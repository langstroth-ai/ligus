import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getWorkspaceCliUserConfig, setWorkspaceCliUserConfig } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { ErrorState, Hint, LoadingState, Section, Switch } from "@/components/ui";

/// Settings → Guards → Your own agent setup: whether this workspace's CLI
/// agents (Claude Code today) run with the user's own settings, hooks,
/// plugins and skills. Off by default, on this device only.
export function CliUserConfigPanel() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["cli-user-config"], queryFn: getWorkspaceCliUserConfig });
  const m = useMutation({
    mutationFn: (on: boolean) => setWorkspaceCliUserConfig(on),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["cli-user-config"] }),
    onError: (e) => toast.error(errMsg(e)),
  });
  return (
    <Section
      title="Your own agent setup"
      info="Claude Code turns get only what Ligus gives them: Ligus's tools, the chat's MCP servers and the built-in file, shell and web tools. Turn this on to also load your own Claude Code settings, hooks, plugins and skills in this workspace. Cloud routines, scheduled tasks, background monitors and push notifications stay off either way, and this is ignored while a chat is locked down or the workspace is in dearth mode."
    >
      <Hint>If on, your hooks and plugins run inside the turn&apos;s sandbox.</Hint>
      {q.isError && <ErrorState text="Couldn't load this setting." onRetry={() => void q.refetch()} />}
      {q.isPending && <LoadingState rows={1} label="Loading" />}
      {q.data !== undefined && (
        <label className="flex items-center justify-between gap-3 text-[13px]" data-testid="cli-user-config">
          <span>Use my Claude Code settings, plugins and skills in this workspace</span>
          <Switch on={q.data} onChange={(on) => m.mutate(on)} disabled={m.isPending} label="Use my Claude Code settings, plugins and skills in this workspace" />
        </label>
      )}
    </Section>
  );
}
