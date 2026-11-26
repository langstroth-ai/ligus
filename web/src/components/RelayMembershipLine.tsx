// The People pane's one line about relay-side membership: what the relay
// checks for this workspace, what Ligus checks regardless, and the one next
// step that can actually work from this device (claim it, get a relay token
// that names you, or nothing).

import { Button, InfoTip } from "@/components/ui";
import type { RelayMembershipDto } from "@/lib/ipc";

/// What Ligus enforces whatever the relay does. Kept in one place so every
/// state says the same thing.
export const LIGUS_ENFORCES =
  "Everything in the workspace is end-to-end encrypted and signed by its members, so the relay can't read it and a stranger can't read it, pose as a member or change anything. At most someone who knows the workspace id can make the relay store junk, which every device drops.";

export type RelayMembershipCopy = {
  /// The one visible line.
  line: string;
  /// The "?" text.
  detail: string;
  /// The one action this device can take, if any.
  action: "claim" | "token" | null;
};

/// The line, its details and its action for a relay membership state, or
/// `null` when the relay enforces a roster (the Members section's tag says so).
export function relayMembershipCopy(s: RelayMembershipDto): RelayMembershipCopy | null {
  if (s.state === "claimed") return null;
  if (s.state === "hidden") {
    return {
      line: "Relay roster: you're not on it",
      detail: `This workspace is claimed on the relay and this device's relay token doesn't name one of its members, so the relay refuses this device's reads and writes. An owner or admin of the relay roster can add your account. ${LIGUS_ENFORCES}`,
      action: null,
    };
  }
  if (s.state === "off") {
    return {
      line: "Relay doesn't check membership",
      detail: `This relay has membership turned off, so it accepts writes to this workspace from anyone who knows its id. ${LIGUS_ENFORCES} There is nothing to claim on this relay. The relay's own roster (LANGSTROTH_RELAY_MEMBERSHIP=1) is deprecated for Ligus workspaces: who can write is decided by the signed roster on every member's device.`,
      action: null,
    };
  }
  // Unclaimed: the relay could enforce a roster once someone claims it.
  const base = `Nobody has claimed this workspace on the relay, so it accepts writes from anyone who knows its id. ${LIGUS_ENFORCES}`;
  if (s.identified) {
    return {
      line: "Relay doesn't check membership",
      detail: `${base} Claiming makes you its owner on the relay, which then refuses reads and writes from any device whose relay token doesn't name a member.`,
      action: "claim",
    };
  }
  if (s.community) {
    return {
      line: "Relay doesn't check membership",
      detail: `${base} Claiming needs a relay token that names your account, and the community relay has no sign-in: only its operator can issue one. Self-host a relay to manage its roster yourself.`,
      action: null,
    };
  }
  return {
    line: "Relay doesn't check membership",
    detail: `${base} To claim it, this device needs a relay token that names your account: sign in to the relay under Settings → Team sync, or paste a token from its operator there.`,
    action: "token",
  };
}

export function RelayMembershipLine({
  status,
  claiming,
  onClaim,
  onToken,
}: {
  status: RelayMembershipDto;
  claiming: boolean;
  onClaim: () => void;
  onToken: () => void;
}) {
  const copy = relayMembershipCopy(status);
  if (!copy) return null;
  return (
    <div className="flex items-center gap-1.5 px-1 text-[11.5px] leading-5" style={{ color: "var(--ligus-ink-soft)" }}>
      <span>{copy.line}</span>
      <InfoTip label="relay membership" text={copy.detail} />
      <span className="flex-1" />
      {copy.action === "claim" && (
        <Button size="sm" variant="quiet" disabled={claiming} onClick={onClaim}>
          {claiming ? "…" : "Claim ownership"}
        </Button>
      )}
      {copy.action === "token" && (
        <Button size="sm" variant="quiet" onClick={onToken}>
          Add a relay token
        </Button>
      )}
    </div>
  );
}
