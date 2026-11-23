import { useQuery } from "@tanstack/react-query";
import { StatusBar, StatusBarItem } from "@/components/kit";
import { GrowthSuggestions } from "@/components/guards/GrowthSuggestions";
import { UpdateStatusItem } from "@/components/UpdateBanner";
import { useErrorLog } from "@/components/Toast";
import { IconAlertTriangle, IconLock, IconStateNeedsYou, IconStateQueued, IconStateWorking } from "@/lib/icons";
import { listQueuedWork, syncStatus } from "@/lib/ipc";
import { deriveSyncPill } from "@/lib/syncState";
import { needsYouLabel } from "@/lib/needsYou";
import { useLiveTurns } from "@/state/liveTurns";

/// How many chats have an agent working right now.
export function workingCount(live: ReadonlyMap<string, { live: boolean }>): number {
  let n = 0;
  for (const l of live.values()) if (l.live) n += 1;
  return n;
}

/// The quiet strip at the foot of the canvas: state that doesn't need you
/// (sync and encryption, agents working, queued work, an update, a tip) and,
/// only when they'd otherwise be out of sight, the needs-you count and a
/// restricted workspace. Nothing in it blinks or takes the top of the window.
export function AppStatusBar({
  hidden = false,
  onOpenSettings,
  onOpenReview,
  onShowErrors,
  inboxCount = 0,
  onOpenInbox,
  dearthAside = false,
}: {
  /// Focus mode: hidden but kept mounted.
  hidden?: boolean;
  onOpenSettings: (tab: string) => void;
  /// Queued work is listed on the Review pane.
  onOpenReview: () => void;
  onShowErrors: () => void;
  /// Shown only when the sidebar (and its Inbox row) is out of sight.
  inboxCount?: number;
  onOpenInbox?: () => void;
  /// The workspace is restricted but a chat's lockdown holds the one banner.
  dearthAside?: boolean;
}) {
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus, refetchInterval: 15_000 });
  const queued = useQuery({ queryKey: ["queued-work"], queryFn: listQueuedWork, refetchInterval: 30_000 });
  const live = useLiveTurns();
  const errors = useErrorLog();
  const pill = deriveSyncPill(sync.data);
  const tone = pill.color === "success" ? "success" : pill.color ?? "neutral";
  const working = workingCount(live);
  const queuedN = queued.data?.length ?? 0;
  const encrypted = Boolean(sync.data?.encrypted) && sync.data?.connectionState === "live";

  return (
    <StatusBar
      hidden={hidden}
      start={
        <>
          <StatusBarItem
            icon={
              pill.color === "danger" || pill.color === "warn" ? (
                <IconAlertTriangle size={11} />
              ) : encrypted ? (
                <IconLock size={11} />
              ) : (
                <span className="inline-block h-1.5 w-1.5 rounded-full" style={{ background: "currentColor" }} />
              )
            }
            tone={tone}
            onClick={() => onOpenSettings("team-sync")}
            describe={[pill.title, encrypted ? "End-to-end encrypted" : null].filter(Boolean).join(". ")}
            live
          >
            {pill.label}
          </StatusBarItem>
          {working > 0 && (
            <StatusBarItem icon={<IconStateWorking size={11} />} tone="info">
              {working === 1 ? "1 agent working" : `${working} agents working`}
            </StatusBarItem>
          )}
          {queuedN > 0 && (
            <StatusBarItem icon={<IconStateQueued size={11} />} onClick={onOpenReview} describe="Waiting for its host; listed on the Review pane">
              {queuedN} queued
            </StatusBarItem>
          )}
          {dearthAside && (
            <StatusBarItem icon={<IconLock size={11} />} tone="warn" onClick={() => onOpenSettings("guards")} describe="The workspace is in dearth mode">
              Dearth mode
            </StatusBarItem>
          )}
          <GrowthSuggestions onOpen={(kind) => onOpenSettings(kind === "agent" ? "models" : "team-sync")} />
        </>
      }
      end={
        <>
          {errors.unseen > 0 && (
            <StatusBarItem icon={<IconAlertTriangle size={11} />} tone="danger" onClick={onShowErrors} describe="Open recent errors">
              {errors.unseen === 1 ? "1 error" : `${errors.unseen} errors`}
            </StatusBarItem>
          )}
          <UpdateStatusItem />
          {onOpenInbox && inboxCount > 0 && (
            <StatusBarItem icon={<IconStateNeedsYou size={11} />} onClick={onOpenInbox} describe="Open the Inbox">
              {needsYouLabel(inboxCount)}
            </StatusBarItem>
          )}
        </>
      }
    />
  );
}
