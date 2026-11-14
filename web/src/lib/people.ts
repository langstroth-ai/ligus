import { confirmDialog } from "@/components/Dialog";
import type { InviteTrust } from "@/lib/ipc";

/// What an invite's sender check means, in words. Names are never identity:
/// the safety code is what tells two people apart.
export function trustNote(trust: InviteTrust, founderName: string | null): string {
  switch (trust) {
    case "known":
      return `From ${founderName || "someone you share a workspace with"}: signed by their device, and their safety code matches.`;
    case "claimed":
      return `Says it's from ${founderName || "someone you share a workspace with"}, but it isn't signed by their device, so nothing shows who sent it. Accept only if you expected it.`;
    case "mismatch":
      return `This invite claims to be from ${founderName || "a co-member"}, but its safety code doesn't match theirs. Don't accept it.`;
    case "unknown":
      return "From someone you don't share a workspace with yet. Compare their safety code with them before you accept.";
    default:
      return "This invite carries no safety code, so nothing confirms who sent it.";
  }
}

/// Ask the person to compare a stranger's safety code out of band before a
/// DM is accepted. Resolves true only if they confirm they compared it.
export function confirmSafetyCode(code: string | null): Promise<boolean> {
  return confirmDialog(
    `Ask the sender to read you their safety code (in person or on a call) and check it matches:\n\n${code ?? "(none)"}\n\nAccept only if it matches.`,
    { title: "Compare safety codes", confirmLabel: "It matches" },
  );
}
