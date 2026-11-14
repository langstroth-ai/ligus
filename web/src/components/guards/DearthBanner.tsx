import { useQuery } from "@tanstack/react-query";
import { getDearthState, type DearthStateDto } from "@/lib/ipc";
import { IconClock, IconLock } from "@/lib/icons";
import { Badge, Banner, LinkButton } from "@/components/ui";

/// The one-line summary the banner shows. Exported for tests.
export function dearthSummary(d: DearthStateDto): string {
  const mcp = d.allowedMcpServers.length;
  const parts = [
    "new members and devices need two approvals",
    mcp === 0 ? "agents have no network tools" : `agents reach only ${mcp} allow-listed MCP server${mcp === 1 ? "" : "s"}`,
    "changes to protected paths need a human",
  ];
  return parts.join(" · ");
}

/// Visible while the workspace is in dearth mode (a tight entrance). Polls,
/// and refreshes on sync through the shared query key. Manage jumps straight
/// to Settings → Guards.
export function DearthBanner({ onManage }: { onManage: () => void }) {
  const q = useQuery({ queryKey: ["dearth-state"], queryFn: getDearthState, refetchInterval: 30000 });
  const d = q.data;
  if (!d?.enabled) return null;
  const pending = d.pending.length;
  return (
    <Banner
      tone="warn"
      testId="dearth-banner"
      icon={<IconLock size={14} />}
      title={
        <>
          Dearth mode
          {d.reason && <span className="ml-1.5 font-normal text-ink-soft">({d.reason})</span>}
        </>
      }
      detail={<span title={dearthSummary(d)}>{dearthSummary(d)}</span>}
      actions={
        <>
          {pending > 0 && (
            <Badge tone="warn" icon={<IconClock size={11} />}>
              {pending} waiting for approval
            </Badge>
          )}
          <LinkButton onClick={onManage} title="Open Settings → Guards">
            Manage
          </LinkButton>
        </>
      }
    />
  );
}
