// Invites for a workspace with no relay. Joining goes through the relay (a
// box's `ligus enroll` refuses an invite without one, and a desktop that
// joined it would never sync), so the desktop's workspace_invite refuses a
// relayless workspace with a typed error. This maps that error to a plain
// message and a way to Settings → Team sync, where the relay is set.
import { errMsg } from "@/components/Toast";
import { openSettingsTab } from "./nav";

/** The prefix of workspace_invite's error for a workspace with no relay. */
export const NO_RELAY_ERR = "workspace-no-relay";

export const NO_RELAY_MESSAGE =
  "This workspace has no relay, so an invite to it wouldn't work. Set a Relay URL under Settings → Team sync, then copy a new invite.";

export function isNoRelayError(e: unknown): boolean {
  return e != null && errMsg(e).startsWith(NO_RELAY_ERR);
}

/** What to show when an invite (or short code) couldn't be made. */
export function inviteError(e: unknown): { message: string; action?: { label: string; run: () => void } } {
  if (isNoRelayError(e)) {
    return { message: NO_RELAY_MESSAGE, action: { label: "Open Team sync", run: () => openSettingsTab("team-sync") } };
  }
  return { message: errMsg(e), action: undefined };
}
