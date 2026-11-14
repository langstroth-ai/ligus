import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { clearChatLockdown, getChatLockdown, type LockdownSignalDto } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { IconAlertTriangle } from "@/lib/icons";

/// Human wording for a lockdown signal. Exported for tests.
export function signalLabel(s: LockdownSignalDto): string {
  switch (s.signal) {
    case "injectionDetected":
      return `prompt-injection marker in a tool result (${s.detail})`;
    case "secretInDiff":
      return `credential in an agent's changes (${s.detail})`;
    case "ciConfigChanged":
      return `unrequested CI configuration change (${s.detail})`;
    default:
      return "locked by a member";
  }
}

/// Shown under the chat header while the chat is locked down: why, what it
/// restricts, and Clear for a human member.
export function LockdownBar({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["chat-lockdown", sessionId],
    queryFn: () => getChatLockdown(sessionId),
    refetchInterval: 15000,
  });
  const clear = useMutation({
    mutationFn: () => clearChatLockdown(sessionId),
    onSuccess: () => {
      toast.success("Lockdown cleared");
      qc.invalidateQueries({ queryKey: ["chat-lockdown", sessionId] });
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const d = q.data;
  if (!d?.active) return null;
  return (
    <div
      role="alert"
      data-testid="lockdown-bar"
      className="flex shrink-0 flex-wrap items-center gap-2 border-b px-4 py-1.5 text-[12px]"
      style={{
        borderColor: "var(--hive-line)",
        background: "color-mix(in srgb, var(--hive-danger) 12%, var(--hive-panel))",
        color: "var(--hive-ink)",
      }}
    >
      <span style={{ color: "var(--hive-danger-ink)" }}>
        <IconAlertTriangle size={14} />
      </span>
      <span className="font-semibold">Locked down</span>
      <span className="min-w-0 truncate opacity-80">{d.signals.map(signalLabel).join("; ")}</span>
      <span className="opacity-60">· no network for agents · every proposal needs a human</span>
      <span className="flex-1" />
      {d.canClear ? (
        <button
          type="button"
          className="font-medium underline-offset-2 hover:underline"
          disabled={clear.isPending}
          onClick={() => clear.mutate()}
        >
          Clear lockdown
        </button>
      ) : (
        <span className="opacity-60">a member can clear it</span>
      )}
    </div>
  );
}
