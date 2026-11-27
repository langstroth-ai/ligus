import { useQuery } from "@tanstack/react-query";
import { inviteLinkHint } from "@/lib/ipc";

/// The invite guard: one line on a join screen when the pasted invite or
/// short code looks like it is for someone who already uses Ligus on another
/// device. A hint only: joining still works, and invites stay for other people.
export function InviteLinkHint({ code, onLink }: { code: string; onLink: () => void }) {
  const c = code.trim();
  const hint = useQuery({
    queryKey: ["invite-link-hint", c],
    queryFn: () => inviteLinkHint(c),
    enabled: c.length > 0,
    staleTime: 60_000,
  });
  if (!c || hint.data !== true) return null;
  return (
    <p className="text-xs text-ink-soft" data-testid="invite-link-hint">
      Looks like you already use Ligus.{" "}
      <button type="button" className="underline hover:text-ink" onClick={onLink}>
        Link this device instead
      </button>
    </p>
  );
}
