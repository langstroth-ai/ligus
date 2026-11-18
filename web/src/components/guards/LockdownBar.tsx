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

/// Scroll the transcript to the tool call that tripped the lockdown, open it,
/// and flash it. With the cause's message id (newer lockdowns carry it) the
/// search stays inside that reply: its tool call named in the signal detail
/// (e.g. "fetch_url result"), else its last tool call, else the reply itself.
/// Without one, it matches by tool name across the chat and falls back to the
/// newest tool call. Returns false if the cause isn't shown.
export function revealLockdownCause(detail: string, messageId?: string): boolean {
  const name = detail.split(/\s+/)[0] ?? "";
  let scope: ParentNode = document;
  let turn: HTMLElement | null = null;
  if (messageId) {
    turn = document.querySelector<HTMLElement>(`.tt[data-message-id="${CSS.escape(messageId)}"]`);
    if (!turn) return false;
    scope = turn;
  }
  const cards = Array.from(scope.querySelectorAll<HTMLDetailsElement>("details.tcall"));
  const card =
    [...cards].reverse().find((c) => c.querySelector(".tcall-n")?.textContent?.trim() === name) ?? cards[cards.length - 1];
  const target: HTMLElement | null = card ?? turn;
  if (!target) return false;
  if (card) card.open = true;
  target.scrollIntoView({ block: "center", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  target.classList.remove("hive-flash");
  void target.offsetWidth;
  target.classList.add("hive-flash");
  if (card) (card.querySelector("summary") as HTMLElement | null)?.focus({ preventScroll: true });
  return true;
}

/// Shown under the chat header while the chat is locked down: why, what it
/// restricts, a jump to the cause, and Clear for a human member. Clear lifts
/// the signals shown here, by their raise ids: one raised since the bar last
/// refreshed stays in force (and shows up on the next refresh).
export function LockdownBar({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["chat-lockdown", sessionId],
    queryFn: () => getChatLockdown(sessionId),
    refetchInterval: 15000,
  });
  const clear = useMutation({
    mutationFn: (raiseIds: string[]) => clearChatLockdown(sessionId, raiseIds),
    onSuccess: () => {
      toast.success("Lockdown cleared");
      qc.invalidateQueries({ queryKey: ["chat-lockdown", sessionId] });
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const d = q.data;
  if (!d?.active) return null;
  const why = d.signals.map(signalLabel).join("; ");
  const shown = d.signals.flatMap((s) => (s.raiseEventId ? [s.raiseEventId] : []));
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
                if (!revealLockdownCause(cause.detail, cause.messageId)) toast.info("The cause isn't in the loaded part of this chat.");
              }}
            >
              Show cause
            </LinkButton>
          )}
          {d.canClear && shown.length > 0 ? (
            <LinkButton disabled={clear.isPending} onClick={() => clear.mutate(shown)}>
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
