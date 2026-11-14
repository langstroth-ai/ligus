import type React from "react";
import { Badge } from "@/components/ui";
import { IconAlertTriangle, IconLock, IconShield, IconUsers, IconWrench, IconSparkle, IconFile, IconMessage, IconFlow } from "@/lib/icons";
import type { ProposalDto } from "@/lib/ipc";

/// What a proposal is, who made it and what it touches (Track D4). Every
/// badge carries an icon and text, never colour alone.
export function ProposalBadges({ proposal }: { proposal: ProposalDto }) {
  const kind = proposal.workflow ? "workflow" : proposal.kind;
  const kindLabel: Record<string, string> = { fileDiff: "File change", command: "Command", decision: "Decision", workflow: "Workflow" };
  const kindIcon: Record<string, React.JSX.Element> = {
    fileDiff: <IconFile size={11} />,
    command: <IconWrench size={11} />,
    decision: <IconMessage size={11} />,
    workflow: <IconFlow size={11} />,
  };
  const agent = proposal.authorKind === "agent";
  const signed =
    proposal.signedBy === "agent" ? "agent-signed" : proposal.signedBy === "host" ? "host-signed (legacy)" : null;
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-1 pl-[22px]" data-testid="proposal-badges">
      <Badge icon={kindIcon[kind]} title="What Implement does with it">
        {kindLabel[kind] ?? kind}
      </Badge>
      {proposal.authorName && (
        <Badge
          tone={agent ? "info" : "neutral"}
          icon={agent ? <IconSparkle size={11} /> : <IconUsers size={11} />}
          title={agent ? "Written by an agent" : "Written by a person"}
        >
          {proposal.authorName}
          {agent && signed ? ` · ${signed}` : ""}
        </Badge>
      )}
      {proposal.configImpact && (
        <Badge tone="warn" icon={<IconAlertTriangle size={11} />} title="Touches configuration, CI, hooks, protected dirs or MCP config">
          Config impact
        </Badge>
      )}
      {proposal.humanGate && (
        <Badge tone="warn" icon={<IconUsers size={11} />} title="A person's own approval is required">
          Human gate
        </Badge>
      )}
      {proposal.presenceRequired && (
        <Badge tone="info" icon={<IconShield size={11} />} title="Applying it needs a passkey">
          Presence required
        </Badge>
      )}
      {proposal.legacyCommand && (
        <Badge tone="warn" icon={<IconLock size={11} />} title="No exact command was filed: Implement sends an instruction to an agent instead">
          No exact command
        </Badge>
      )}
    </div>
  );
}
