import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { clearChatLockdown, getChatLockdown, type LockdownSignalDto } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { IconAlertTriangle } from "@/lib/icons";
import { Banner, LinkButton } from "@/components/ui";

/// Human wording for a lockdown signal. Exported for tests.
export function signalLabel(s: LockdownSignalDto): string {
  switch (s.signal) {
    case "injectionDetected":
      return `prompt-injection marker in a tool result (${s.detail})`;
    case "secretInDiff":
      return `credential in an agent's changes (${s.detail})`;
    case "ciConfigChanged":
      return `unrequested CI configuration change (${s.detail})`;
    default:
      return "locked by a member";
  }
}

/// Scroll the transcript to the tool call that tripped the lockdown (matched by
/// tool name from the signal detail, e.g. "fetch_url result"), open it, and
/// flash it. Falls back to the newest tool call. Returns false if none is shown.
export function revealLockdownCause(detail: string): boolean {
  const name = detail.split(/\s+/)[0] ?? "";
  const cards = Array.from(document.querySelectorAll<HTMLDetailsElement>("details.tcall"));
  const match =
    [...cards].reverse().find((c) => c.querySelector(".tcall-n")?.textContent?.trim() === name) ?? cards[cards.length - 1];
  if (!match) return false;
  match.open = true;
  match.scrollIntoView({ block: "center", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  match.classList.remove("hive-flash");
  void match.offsetWidth;
  match.classList.add("hive-flash");
  (match.querySelector("summary") as HTMLElement | null)?.focus({ preventScroll: true });
  return true;
}

/// Shown under the chat header while the chat is locked down: why, what it
/// restricts, a jump to the cause, and Clear for a human member.
export function LockdownBar({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["chat-lockdown", sessionId],
    queryFn: () => getChatLockdown(sessionId),
    refetchInterval: 15000,
  });
  const clear = useMutation({
    mutationFn: () => clearChatLockdown(sessionId),
    onSuccess: () => {
      toast.success("Lockdown cleared");
      qc.invalidateQueries({ queryKey: ["chat-lockdown", sessionId] });
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const d = q.data;
  if (!d?.active) return null;
  const why = d.signals.map(signalLabel).join("; ");
  const cause = d.signals.find((s) => s.signal !== "manual" && s.detail);
  return (
    <Banner
      tone="danger"
      role="alert"
      testId="lockdown-bar"
      icon={<IconAlertTriangle size={14} />}
      title="Locked down"
      detail={
        <span title={`${why}. No network for agents; every proposal needs a human.`}>
          {why}
          <span className="text-ink-soft"> · no network for agents · every proposal needs a human</span>
        </span>
      }
      actions={
        <>
          {cause && (
            <LinkButton
              tone="ink"
              onClick={() => {
                if (!revealLockdownCause(cause.detail)) toast.info("The cause isn't in the loaded part of this chat.");
              }}
            >
              Show cause
            </LinkButton>
          )}
          {d.canClear ? (
            <LinkButton disabled={clear.isPending} onClick={() => clear.mutate()}>
              Clear lockdown
            </LinkButton>
          ) : (
            <span className="text-xs text-ink-soft">a member can clear it</span>
          )}
        </>
      }
    />
  );
}
