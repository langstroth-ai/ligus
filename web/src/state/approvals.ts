import { useEffect, useSyncExternalStore } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { listApprovals, onApprovalSession, onApprovalsChanged, type ApprovalsDto } from "@/lib/ipc";

// Code-match approval requests (docs/design/code-match-approval.md 7.2): the
// listing the background loop keeps (["approvals"]), and which request's
// sheet is open, app-wide so the Inbox, Admit a box and Add a computer open
// the same sheet.

export const APPROVALS_KEY = ["approvals"] as const;
export const sessionKey = (rid: string) => ["approval-session", rid] as const;

type Open = { workspaceId: string; rid: string } | null;
let open: Open = null;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

export const approvalSheet = {
  open(workspaceId: string, rid: string) {
    open = { workspaceId, rid };
    emit();
  },
  close() {
    open = null;
    emit();
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
  snapshot: (): Open => open,
};

export function useApprovalSheet(): Open {
  return useSyncExternalStore(approvalSheet.subscribe, approvalSheet.snapshot, approvalSheet.snapshot);
}

const EMPTY: ApprovalsDto = { requests: [], facts: [] };

/// The live requests and the day's facts (every workspace: callers filter).
/// `loaded`: the listing has been read at least once.
export function useApprovals(): ApprovalsDto & { loaded: boolean } {
  const q = useQuery({ queryKey: APPROVALS_KEY, queryFn: listApprovals, refetchInterval: 15_000, retry: false });
  return { requests: q.data?.requests ?? EMPTY.requests, facts: q.data?.facts ?? EMPTY.facts, loaded: q.isFetched };
}

/// Follow the backend's events (call once, at the app root).
export function useApprovalEvents() {
  const qc = useQueryClient();
  useEffect(() => {
    const a = onApprovalsChanged(() => void qc.invalidateQueries({ queryKey: APPROVALS_KEY }));
    const b = onApprovalSession((s) => qc.setQueryData(sessionKey(s.rid), s));
    return () => {
      void a.then((un) => un()).catch(() => {});
      void b.then((un) => un()).catch(() => {});
    };
  }, [qc]);
}
