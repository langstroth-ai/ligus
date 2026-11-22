import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  chatWorkspaceRepo,
  getAppSettings,
  getGitStatus,
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

/// A chat worktree branch ("ligus/chat/<uuid>") shortened to "chat/7f3a1c2e";
/// any other branch name is kept as is.
export function shortBranch(branch: string): string {
  const m = branch.match(/(?:^|\/)chat\/([0-9a-f]{8})[0-9a-f-]{28}$/i);
  return m ? `chat/${m[1]}` : branch;
}

/// The pieces of the "runs on" line, in reading order: host, runtime, branch.
/// Missing pieces are left out rather than guessed.
export function runsOnParts(p: { host: string; runtime: string | null; branch: string | null }): string[] {
  const parts = [`Runs on ${p.host.trim() || "this device"}`];
  if (p.runtime?.trim()) parts.push(p.runtime.trim());
  if (p.branch?.trim()) parts.push(shortBranch(p.branch.trim()));
  return parts;
}

/// One quiet line under the chat title: where this chat's turns run (this
/// device), with which runtime, on which branch (the chat's own worktree
/// branch when it has one).
export function ChatRunsOn({ sessionId, runtime }: { sessionId: string; runtime: string | null }) {
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  // The backend points git at the open chat's worktree; keying on that root
  // refetches once the switch lands instead of showing the last chat's branch.
  const root = settings.data?.workspaceRoot ?? "";
  const git = useQuery({
    queryKey: ["git-status", "chat", sessionId, root],
    queryFn: getGitStatus,
    enabled: root.trim().length > 0,
    staleTime: 15_000,
  });
  const branch = git.data?.branch ?? null;
  const parts = runsOnParts({ host: settings.data?.deviceName ?? "", runtime, branch });
  return (
    <div
      data-testid="chat-runs-on"
      className="flex min-w-0 items-center gap-1.5 truncate text-[11px] leading-4"
      style={{ color: "var(--ligus-ink-soft)" }}
      title={branch && branch !== shortBranch(branch) ? `Branch ${branch}` : undefined}
    >
      {parts.map((p, i) => (
        <span key={i} className={i === 0 ? "shrink-0" : "min-w-0 truncate"}>
          {i > 0 && (
            <span aria-hidden className="mr-1.5 text-ink-soft">
              ·
            </span>
          )}
          {i === parts.length - 1 && branch && parts.length > 1 ? (
            <>
              <span aria-hidden className="mr-0.5">⎇</span>
              <span className="sr-only">branch </span>
              {p}
            </>
          ) : (
            p
          )}
        </span>
      ))}
    </div>
  );
}
