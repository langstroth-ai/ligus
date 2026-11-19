import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { chatWorkspaceRepo, listWorkspaceRepos, setChatWorkspaceRepo, type ChatWorkspaceRepoDto } from "@/lib/ipc";
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
  const label = current ? `${current.name ?? "Unknown repository"}${current.baseBranch ? ` · ${current.baseBranch}` : ""}` : "No repository";
  return (
    <div className="relative flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs" style={{ borderColor: "var(--hive-line)" }}>
      <button
        type="button"
        aria-label="Chat repository"
        className="flex items-center gap-1 rounded-full border px-2 py-0.5"
        style={{ borderColor: "var(--hive-line)" }}
        onClick={() => setOpen((o) => !o)}
      >
        {warning && <span aria-hidden style={{ color: "var(--hive-accent-warm, orange)" }}>●</span>}
        {label}
      </button>
      {warning && <span className="text-ink-soft">{warning}</span>}
      {open && (
        <div
          role="menu"
          className="absolute left-3 top-8 z-20 min-w-56 rounded-xl border p-1 shadow"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        >
          {live.map((r) => (
            <button key={r.id} role="menuitem" type="button" className="block w-full rounded-lg px-2 py-1 text-left hover:bg-black/5" onClick={() => void pick(r.id)}>
              {r.name} <span className="text-ink-soft">{r.canonical}</span>
            </button>
          ))}
          <button role="menuitem" type="button" className="block w-full rounded-lg px-2 py-1 text-left hover:bg-black/5" onClick={() => void pick(null)}>
            No repository
          </button>
        </div>
      )}
    </div>
  );
}
