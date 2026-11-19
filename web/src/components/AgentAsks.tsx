import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button } from "@/components/ui";
import { answerAsk, listAsks, type AskDto } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";

/// Questions agents asked in this chat (`ligus_ask_human`). The asking turn
/// is paused until someone answers or its timeout passes. Answering
/// authorizes nothing by itself; the first answer wins.
export function AgentAsks({ sessionId }: { sessionId: string }) {
  const asks = useQuery({ queryKey: ["asks", sessionId], queryFn: () => listAsks(sessionId), refetchInterval: 4000 });
  const open = (asks.data ?? []).filter((a) => a.open);
  if (open.length === 0) return null;
  return (
    <div className="mb-4 flex flex-col gap-2" data-testid="agent-asks">
      {open.map((a) => (
        <AskCard key={a.askId} sessionId={sessionId} ask={a} />
      ))}
    </div>
  );
}

function AskCard({ sessionId, ask }: { sessionId: string; ask: AskDto }) {
  const qc = useQueryClient();
  const [text, setText] = useState("");
  const answer = useMutation({
    mutationFn: (v: { text: string; option: number | null }) => answerAsk(sessionId, ask.askId, v.text, v.option),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["asks", sessionId] }),
    onError: (e) => toast.error(`Couldn't send your answer: ${errMsg(e)}`),
  });
  return (
    <div className="rounded-xl border px-3 py-2" style={{ borderColor: "var(--ligus-line)" }} data-testid="ask-card">
      <div className="flex items-center gap-2">
        <Badge tone="info">{ask.agentName} asks</Badge>
        <span className="text-xs text-ink-soft">its turn is waiting</span>
      </div>
      <p className="mt-1.5 text-sm">{ask.question}</p>
      {ask.options.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1.5">
          {ask.options.map((o, i) => (
            <Button key={o} disabled={answer.isPending} onClick={() => answer.mutate({ text: "", option: i })}>
              {o}
            </Button>
          ))}
        </div>
      )}
      <form
        className="mt-2 flex gap-1.5"
        onSubmit={(e) => {
          e.preventDefault();
          if (text.trim()) answer.mutate({ text: text.trim(), option: null });
        }}
      >
        <input
          aria-label="Your answer"
          className="min-w-0 flex-1 rounded-lg border px-2 py-1 text-sm"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="Answer…"
        />
        <Button type="submit" variant="primary" disabled={!text.trim() || answer.isPending}>
          Answer
        </Button>
      </form>
    </div>
  );
}
