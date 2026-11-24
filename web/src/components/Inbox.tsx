import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AgentAsks } from "@/components/AgentAsks";
import { PendingInviteRows } from "@/components/PendingInvitesBanner";
import { SandboxGrantsNotice } from "@/components/SandboxGrantsBanner";
import { WorkspaceTrustSheet } from "@/components/WorkspaceTrustBanner";
import { RepoSuggestionNotice, REPOS_KEY } from "@/components/settings/RepositoriesSection";
import { InboxItem } from "@/components/kit";
import { Button, SectionCap } from "@/components/ui";
import { listWorkspaceRepos } from "@/lib/ipc";
import { needsYouCount, needsYouLabel, type NeedsYouItem } from "@/lib/needsYou";
import { liveTurns } from "@/state/liveTurns";
import { phoneAlerts } from "@/state/phoneAlerts";

export interface InboxActions {
  openChat: (sessionId: string) => void;
  /// Open a chat on its Review pane (a vote or an apply). Nothing is approved
  /// from the Inbox: the change has to be on screen first.
  openProposal: (sessionId: string, proposalId: string) => void;
  openSettingsTab: (tab: string) => void;
}

/// Everything that needs you, in one place: agents' questions (answered right
/// here), proposals waiting on your vote or ready to apply, runs that failed,
/// a phone code another phone answered, changed sandbox grants, repository
/// commands to enable, boxes asking to be your device, and invites. Below,
/// quieter, suggestions that don't need you. Opening a row takes you to it.
export function Inbox({ items, actions }: { items: readonly NeedsYouItem[]; actions: InboxActions }) {
  const [trustOpen, setTrustOpen] = useState(false);
  const repos = useQuery({ queryKey: REPOS_KEY, queryFn: listWorkspaceRepos, retry: false });
  const n = needsYouCount(items);
  // Rows that render a whole kind at once (they own its data and actions)
  // go where the first item of that kind falls in the order.
  const placed = new Set<string>();
  const once = (kind: string) => {
    if (placed.has(kind)) return false;
    placed.add(kind);
    return true;
  };

  function row(item: NeedsYouItem) {
    const where = item.chatTitle;
    switch (item.kind) {
      case "ask":
        return [
          <InboxItem
            key={item.key}
            state="idle"
            title={item.title}
            context={where}
            onOpen={() => actions.openChat(item.chatId!)}
          />,
          <div key={`${item.key}:answer`} role="listitem" aria-label={`Answer in ${where ?? "the chat"}`} className="px-3 pb-1">
            <AgentAsks sessionId={item.chatId!} />
          </div>,
        ];
      case "vote":
      case "apply":
        return (
          <InboxItem
            key={item.key}
            state="idle"
            title={item.title}
            context={where}
            onOpen={() => actions.openProposal(item.chatId!, item.proposalId!)}
            actions={
              <Button size="sm" onClick={() => actions.openProposal(item.chatId!, item.proposalId!)}>
                Review
              </Button>
            }
          />
        );
      case "failed":
        return (
          <InboxItem
            key={item.key}
            state="failed"
            title={item.title}
            context={where}
            detail={liveTurns.failureOf(item.chatId!) ?? undefined}
            onOpen={() => actions.openChat(item.chatId!)}
            menu={[
              { label: "Open chat", onSelect: () => actions.openChat(item.chatId!) },
              { label: "Dismiss", onSelect: () => liveTurns.clearFailure(item.chatId!) },
            ]}
          />
        );
      case "phone":
        return (
          <InboxItem
            key={item.key}
            state="needsYou"
            title={item.title}
            detail="If the phone you added isn't the one in your hand, revoke it."
            onOpen={() => phoneAlerts.show()}
            actions={
              <Button size="sm" onClick={() => phoneAlerts.show()}>
                Review
              </Button>
            }
          />
        );
      case "trust":
        return (
          <InboxItem
            key={item.key}
            state="idle"
            title={item.title}
            detail="MCP servers and runtimes stay inert until you enable them on this device."
            onOpen={() => setTrustOpen(true)}
            actions={
              <Button size="sm" onClick={() => setTrustOpen(true)}>
                Review
              </Button>
            }
          />
        );
      case "device":
        return (
          <InboxItem
            key={item.key}
            state="idle"
            title={item.title}
            detail="Boxes join as their own member: run ligus admit request on it, then Admit a box."
            onOpen={() => actions.openSettingsTab("account")}
            actions={
              <Button size="sm" onClick={() => actions.openSettingsTab("account")}>
                Review
              </Button>
            }
          />
        );
      case "sandbox":
        return once("sandbox") ? <SandboxGrantsNotice key="sandbox" /> : null;
      case "invite":
        return once("invite") ? <PendingInviteRows key="invites" /> : null;
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto" data-testid="inbox">
      <div className="mx-auto w-full max-w-[760px] px-6 py-6">
        <h1 className="ligus-title" style={{ color: "var(--ligus-ink)" }}>
          Inbox
        </h1>
        <p className="mt-0.5 text-[12.5px] text-ink-soft">{n > 0 ? needsYouLabel(n) : "Nothing needs you."}</p>
        {items.length > 0 && (
          <div role="list" aria-label="Needs you" className="mt-4 flex flex-col divide-y divide-[color:var(--ligus-line)]">
            {items.flatMap((i) => row(i) ?? [])}
          </div>
        )}
        {repos.data?.suggestion && (
          <section aria-label="Suggestions" className="mt-6">
            <SectionCap>Suggestions</SectionCap>
            <div className="mt-1 px-1">
              <RepoSuggestionNotice />
            </div>
          </section>
        )}
      </div>
      {trustOpen && <WorkspaceTrustSheet onClose={() => setTrustOpen(false)} />}
    </div>
  );
}
