import { useQuery } from "@tanstack/react-query";
import { DecisionCard } from "@/components/DecisionCard";
import { listAsks } from "@/lib/ipc";

/// The open questions agents asked in this chat (the Review pane's list).
/// Each is a decision card; the asking turn is paused until it is settled
/// or its timeout passes.
export function AgentAsks({ sessionId }: { sessionId: string }) {
  const asks = useQuery({ queryKey: ["asks", sessionId], queryFn: () => listAsks(sessionId), refetchInterval: 4000 });
  const open = (asks.data ?? []).filter((a) => a.live);
  if (open.length === 0) return null;
  return (
    <div className="mb-4 flex flex-col gap-2" data-testid="agent-asks">
      {open.map((a) => (
        <DecisionCard key={a.askId} sessionId={sessionId} ask={a} />
      ))}
    </div>
  );
}
