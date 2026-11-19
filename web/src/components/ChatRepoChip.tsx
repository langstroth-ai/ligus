import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  chatWorkspaceRepo,
  listWorkspaceRepos,
  repinChatBase,
  setChatWorkspaceRepo,
  type ChatWorkspaceRepoDto,
  type PinnedElsewhere,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { REPOS_KEY } from "@/components/settings/RepositoriesSection";

/// The warning a chat shows when this device can't run a turn in its repo.
export function repoWarning(r: ChatWorkspaceRepoDto | null | undefined, hostLabel: string): string | null {
  if (!r) return null;
  if (r.removed) return `${r.name ?? "This repository"} was removed from this workspace.`;
  switch (r.here.state) {
    case "present":
      return null;
    case "cloning":
      return `${r.name} is still cloning on ${hostLabel}.`;
    default:
      return r.here.reason || `${r.name} isn't checked out on ${hostLabel}.`;
  }
}

/// The chat's repo chip: "WeFit · main" (or "No repository"), a picker of
/// the workspace's repos, and a warning when this device has no checkout of
/// the chat's repo. Hidden while the workspace has no repos and the chat
/// names none.
export function ChatRepoChip({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const repos = useQuery({ queryKey: REPOS_KEY, queryFn: listWorkspaceRepos });
  const mine = useQuery({ queryKey: ["chat-repo", sessionId], queryFn: () => chatWorkspaceRepo(sessionId) });
  const [open, setOpen] = useState(false);
  const live = (repos.data?.repos ?? []).filter((r) => !r.removed);
  const current = mine.data ?? null;
  if (!current && live.length === 0) return null;
  const hostLabel = repos.data?.hostLabel ?? "this device";
  const warning = repoWarning(current, hostLabel);
  const pick = async (repoId: string | null) => {
    setOpen(false);
    try {
      await setChatWorkspaceRepo(sessionId, repoId);
      await qc.invalidateQueries({ queryKey: ["chat-repo", sessionId] });
    } catch (e) {
      toast.error(`Couldn't change the chat's repository: ${errMsg(e)}`);
    }
  };
  const repin = async () => {
    setOpen(false);
    try {
      const to = await repinChatBase(sessionId);
      toast.info(`Moved this chat's base to ${to.slice(0, 12)}. Each host rebuilds the chat's branch there on its next turn.`);
      await qc.invalidateQueries({ queryKey: ["chat-repo", sessionId] });
    } catch (e) {
      toast.error(`Couldn't move the chat's base: ${errMsg(e)}`);
    }
  };
  const label = current ? `${current.name ?? "Unknown repository"}${current.baseBranch ? ` · ${current.baseBranch}` : ""}` : "No repository";
  return (
    <div className="relative flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs" style={{ borderColor: "var(--ligus-line)" }}>
      <button
        type="button"
        aria-label="Chat repository"
        className="flex items-center gap-1 rounded-full border px-2 py-0.5"
        style={{ borderColor: "var(--ligus-line)" }}
        onClick={() => setOpen((o) => !o)}
      >
        {warning && <span aria-hidden style={{ color: "var(--ligus-accent-warm, orange)" }}>●</span>}
        {label}
      </button>
      {warning && <span className="text-ink-soft">{warning}</span>}
      {open && (
        <div
          role="menu"
          className="absolute left-3 top-8 z-20 min-w-56 rounded-xl border p-1 shadow"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}
        >
          {live.map((r) => (
            <button key={r.id} role="menuitem" type="button" className="block w-full rounded-lg px-2 py-1 text-left hover:bg-black/5" onClick={() => void pick(r.id)}>
              {r.name} <span className="text-ink-soft">{r.canonical}</span>
            </button>
          ))}
          <button role="menuitem" type="button" className="block w-full rounded-lg px-2 py-1 text-left hover:bg-black/5" onClick={() => void pick(null)}>
            No repository
          </button>
          {current?.canRepin && !current.removed && (
            <button
              role="menuitem"
              type="button"
              className="mt-1 block w-full rounded-lg border-t px-2 py-1 text-left hover:bg-black/5"
              style={{ borderColor: "var(--ligus-line)" }}
              title={current.baseCommit ? `Pinned at ${current.baseCommit.slice(0, 12)}` : undefined}
              onClick={() => void repin()}
            >
              Move the base to the latest {current.baseBranch || "default branch"}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/// The pinned agent a draft addressed to `routeHandle` goes to, when its
/// host says it has no checkout of the chat's repo; else null.
export function pinnedWarning(r: ChatWorkspaceRepoDto | null | undefined, routeHandle: string): PinnedElsewhere | null {
  if (!r || r.removed || !routeHandle) return null;
  const h = routeHandle.toLowerCase();
  return (r.pinnedElsewhere ?? []).find((p) => p.agentName.toLowerCase() === h) ?? null;
}

/// The composer's warning (repos-and-checkouts 6.1, 6.3): "ScoutBee runs on
/// mac-test, which has no WeFit. Clone there · Run here this time". Asking
/// another host to clone is done from the phone connected to it (review Q9),
/// so "Clone there" says how; "Run here this time" sends the next message's
/// turn to this device instead, once.
export function PinnedHostWarning({
  sessionId,
  routeHandle,
  runHere,
  onRunHere,
}: {
  sessionId: string;
  routeHandle: string;
  runHere: boolean;
  onRunHere: (on: boolean) => void;
}) {
  const mine = useQuery({ queryKey: ["chat-repo", sessionId], queryFn: () => chatWorkspaceRepo(sessionId) });
  const [howTo, setHowTo] = useState(false);
  const r = mine.data ?? null;
  const p = pinnedWarning(r, routeHandle);
  if (!p || !r) return null;
  const repo = r.name ?? "this repository";
  if (runHere) {
    return (
      <div role="status" className="mb-2 flex flex-wrap items-center gap-2 text-xs">
        <span>
          {p.agentName} runs here this time, in this device&rsquo;s {repo}.
        </span>
        <button type="button" className="underline" onClick={() => onRunHere(false)}>
          Undo
        </button>
      </div>
    );
  }
  return (
    <div role="alert" className="mb-2 text-xs">
      <div className="flex flex-wrap items-center gap-2">
        <span aria-hidden style={{ color: "var(--ligus-accent-warm, orange)" }}>
          ●
        </span>
        <span>
          {p.agentName} runs on {p.hostLabel}, which has no {repo}.
        </span>
        <button type="button" className="underline" aria-expanded={howTo} onClick={() => setHowTo((o) => !o)}>
          Clone there
        </button>
        <span aria-hidden>·</span>
        <button
          type="button"
          className="underline disabled:no-underline disabled:opacity-50"
          disabled={!p.canRunHere}
          title={p.canRunHere ? undefined : `This device needs ${repo} checked out and ${p.agentName}'s runtime, and ${p.agentName} must be yours`}
          onClick={() => onRunHere(true)}
        >
          Run here this time
        </button>
      </div>
      {howTo && (
        <div className="mt-1 text-ink-soft">
          Clone it on {p.hostLabel} from the Ligus phone app connected to it (Repositories, then Clone here). This
          desktop doesn&rsquo;t ask other hosts to clone.
        </div>
      )}
    </div>
  );
}
