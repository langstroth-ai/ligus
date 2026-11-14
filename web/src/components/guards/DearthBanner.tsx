import { useQuery } from "@tanstack/react-query";
import { getDearthState, type DearthStateDto } from "@/lib/ipc";
import { IconLock } from "@/lib/icons";

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
/// and refreshes on sync through the shared query key.
export function DearthBanner({ onManage }: { onManage: () => void }) {
  const q = useQuery({ queryKey: ["dearth-state"], queryFn: getDearthState, refetchInterval: 30000 });
  const d = q.data;
  if (!d?.enabled) return null;
  const pending = d.pending.length;
  return (
    <div
      role="status"
      data-testid="dearth-banner"
      className="flex shrink-0 items-center gap-2 border-b px-4 py-1.5 text-[12px]"
      style={{
        borderColor: "var(--hive-line)",
        background: "color-mix(in srgb, var(--hive-warning, #b7791f) 14%, var(--hive-panel))",
        color: "var(--hive-ink)",
      }}
    >
      <span style={{ color: "var(--hive-warn-ink)" }}>
        <IconLock size={14} />
      </span>
      <span className="font-semibold">Dearth mode</span>
      {d.reason && <span className="truncate opacity-80">({d.reason})</span>}
      <span className="hidden min-w-0 truncate opacity-70 md:inline">{dearthSummary(d)}</span>
      <span className="flex-1" />
      {pending > 0 && <span className="opacity-80">{pending} waiting for approval</span>}
      <button type="button" className="font-medium underline-offset-2 hover:underline" onClick={onManage}>
        Manage
      </button>
    </div>
  );
}
