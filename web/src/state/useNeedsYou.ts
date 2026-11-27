import { useEffect, useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { getSandboxPolicy, getWorkspaceTrust, listChats, listDeviceRequests, listPendingInvites, setNeedsYouCount } from "@/lib/ipc";
import { needsYouCount, needsYouItems, type NeedsYouItem } from "@/lib/needsYou";
import { useChatAttention } from "@/state/useSessionStates";
import { useLiveTurns } from "@/state/liveTurns";
import { usePhoneAlerts } from "@/state/phoneAlerts";
import { useApprovals } from "@/state/approvals";

/// Everything waiting on this person in the active workspace, plus what
/// reaches this device from outside it (invites, device requests, a phone
/// code answered twice). The queries share their keys with the panes that
/// show the same data, so nothing is fetched twice.
export function useNeedsYou(workspaceId: string): NeedsYouItem[] {
  const attention = useChatAttention(workspaceId);
  const chats = useQuery({ queryKey: ["chats", workspaceId], queryFn: listChats });
  const live = useLiveTurns();
  const invites = useQuery({ queryKey: ["pending-invites"], queryFn: listPendingInvites, refetchInterval: 20_000 });
  const devices = useQuery({ queryKey: ["device-requests"], queryFn: listDeviceRequests, refetchInterval: 15_000 });
  const trust = useQuery({ queryKey: ["workspace-trust"], queryFn: getWorkspaceTrust, refetchInterval: 30_000 });
  const sandbox = useQuery({ queryKey: ["sandbox-policy", null], queryFn: () => getSandboxPolicy(null), refetchInterval: 30_000 });
  const phone = usePhoneAlerts();
  const approvals = useApprovals();
  return useMemo(() => {
    const titles = new Map((chats.data ?? []).map((c) => [c.id, c.title]));
    // Only chats of this workspace: a failure elsewhere shows there.
    const live2 = new Map([...live].filter(([id]) => titles.has(id)));
    return needsYouItems({
      attention: attention.data,
      live: live2,
      chatTitles: titles,
      invites: invites.data?.invites,
      deviceRequests: devices.data,
      phoneAlerts: phone.map((a) => ({ rid: a.rid, otherDeviceId: a.other.deviceId, addedLabel: a.added.label })),
      trustWaiting: (trust.data?.items ?? []).filter((i) => !i.enabled).length,
      sandboxChanged: Boolean(sandbox.data?.changed),
      // This workspace's requests and facts: the sheet approves in the
      // active workspace only.
      approvals: approvals.requests
        .filter((r) => r.workspaceId === workspaceId)
        .map((r) => ({ workspaceId: r.workspaceId, rid: r.rid, kind: r.kind, label: r.label, createdAt: r.createdAt, summary: r.summary })),
      approvalFacts: approvals.facts.filter((f) => f.workspaceId === workspaceId),
    });
  }, [attention.data, chats.data, live, invites.data, devices.data, trust.data, sandbox.data, phone, approvals.requests, approvals.facts, workspaceId]);
}

/// Mirror the count on the dock icon (macOS) and the tray tooltip. Zero
/// clears the badge.
export function useDockBadge(items: readonly NeedsYouItem[]) {
  const n = needsYouCount(items);
  useEffect(() => {
    void setNeedsYouCount(n).catch(() => {});
  }, [n]);
}
