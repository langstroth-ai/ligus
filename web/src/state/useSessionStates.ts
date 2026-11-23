import { useEffect, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { listChatAttention, listQueuedWork } from "@/lib/ipc";
import { sessionStates, type SessionState } from "@/lib/sessionState";
import { useLiveTurns } from "@/state/liveTurns";

/// Under "chats" so every place that refreshes the chat list (a sync, a
/// finished turn, a rename) refreshes this too.
export const chatAttentionKey = (workspaceId: string) => ["chats", "attention", workspaceId];

/// Per-chat attention for the active workspace. Also refetched when any
/// chat's proposals, questions or lockdown change (a vote, an answer), so a
/// chip clears the moment you act instead of on the next poll.
export function useChatAttention(workspaceId: string, enabled = true) {
  const qc = useQueryClient();
  useEffect(() => {
    if (!enabled) return;
    let timer: number | undefined;
    // Only a real change counts: the questions list polls every few seconds,
    // and structural sharing keeps the same data object when nothing changed.
    const lastData = new Map<string, unknown>();
    const unsub = qc.getQueryCache().subscribe((ev) => {
      if (ev.type !== "updated" || ev.action.type !== "success") return;
      const head = ev.query.queryKey[0];
      if (head !== "proposals" && head !== "asks" && head !== "chat-lockdown") return;
      const seen = lastData.has(ev.query.queryHash);
      const prev = lastData.get(ev.query.queryHash);
      lastData.set(ev.query.queryHash, ev.query.state.data);
      if (seen && prev === ev.query.state.data) return;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void qc.invalidateQueries({ queryKey: ["chats", "attention"] }), 250);
    });
    return () => {
      window.clearTimeout(timer);
      unsub();
    };
  }, [qc, enabled]);
  return useQuery({
    queryKey: chatAttentionKey(workspaceId),
    queryFn: listChatAttention,
    enabled,
    // Agents' questions and proposals can arrive mid-turn on this device
    // without a sync; a slow poll catches those.
    refetchInterval: 15_000,
  });
}

/// The state of each listed chat (absent = idle).
export function useSessionStates(chatIds: readonly string[], workspaceId: string): Map<string, SessionState> {
  const attention = useChatAttention(workspaceId);
  const queued = useQuery({ queryKey: ["queued-work"], queryFn: listQueuedWork, refetchInterval: 30_000 });
  const live = useLiveTurns();
  return useMemo(
    () => sessionStates(chatIds, { attention: attention.data, queued: queued.data, live }),
    [chatIds, attention.data, queued.data, live],
  );
}
