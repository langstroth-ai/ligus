import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { chatOldAgentCopies } from "@/lib/ipc";

const KEY = "ligus.oldAgentCopies.dismissed";

function dismissed(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function dismiss(id: string) {
  try {
    localStorage.setItem(KEY, JSON.stringify([...new Set([...dismissed(), id])].slice(-200)));
  } catch {
    // This device only; a notice that comes back is harmless.
  }
}

/// "This chat has an old copy of @x" (workspace model v3, 11): a chat that
/// Ligus before 0.5 gave its own copy of an agent. On rules 4 the chat runs
/// the workspace's agent anyway; an Admin sees this once per chat and agent,
/// and **Use the workspace's @x** only hides it on this device (it writes
/// nothing).
export function OldAgentCopyNotice({ sessionId }: { sessionId: string }) {
  const copies = useQuery({ queryKey: ["old-agent-copies", sessionId], queryFn: () => chatOldAgentCopies(sessionId), staleTime: 60_000 });
  const [hidden, setHidden] = useState<string[]>(dismissed);
  const shown = (copies.data ?? []).filter((c) => !hidden.includes(`${sessionId}:${c.id}`));
  if (shown.length === 0) return null;
  const c = shown[0];
  const hide = () => {
    const id = `${sessionId}:${c.id}`;
    dismiss(id);
    setHidden((h) => [...h, id]);
  };
  return (
    <div role="status" className="mb-2 flex items-center gap-2 text-xs text-ink-soft" data-testid="old-agent-copy">
      <span className="min-w-0 truncate">This chat has an old copy of @{c.name}.</span>
      <button type="button" className="shrink-0 underline" onClick={hide}>
        Use the workspace&rsquo;s @{c.name}
      </button>
    </div>
  );
}
