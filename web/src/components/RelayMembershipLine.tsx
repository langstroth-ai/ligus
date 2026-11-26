// The People pane's one line about relay-side membership: what the relay
// checks for this workspace, what Ligus checks regardless, and the one next
// step that can actually work from this device (claim it, get a relay token
// that names you, or nothing).

import { useState } from "react";
import { Button, InfoTip } from "@/components/ui";
import type { RelayMembershipDto } from "@/lib/ipc";
import { IconX } from "@/lib/icons";

/// This device remembers a hidden line per workspace and state: a change
/// of state (say, the relay now refuses this device) shows it again.
const HIDE_KEY = "ligus.relayMembershipLine.hidden";

function hiddenKey(scope: string, state: string) {
  return `${HIDE_KEY}.${scope}.${state}`;
}

function isHidden(scope: string | undefined, state: string): boolean {
  if (!scope) return false;
  try {
    return localStorage.getItem(hiddenKey(scope, state)) === "1";
  } catch {
    return false;
  }
}

function rememberHidden(scope: string | undefined, state: string) {
  if (!scope) return;
  try {
    localStorage.setItem(hiddenKey(scope, state), "1");
  } catch {
    // Storage blocked: hidden for this session only.
  }
}

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
  scope,
}: {
  status: RelayMembershipDto;
  claiming: boolean;
  onClaim: () => void;
  onToken: () => void;
  /// What a hidden line is remembered under (the workspace).
  scope?: string;
}) {
  const [hidden, setHidden] = useState<string | null>(null);
  const copy = relayMembershipCopy(status);
  if (!copy) return null;
  // The line is informational (Ligus checks membership itself), so it can be
  // put away. "You're not on it" is a refusal this device hits, so it stays.
  const dismissible = status.state !== "hidden";
  if (dismissible && (hidden === status.state || isHidden(scope, status.state))) return null;
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
      {dismissible && (
        <button
          type="button"
          aria-label="Hide this line"
          data-tip="Hide this line"
          className="shrink-0 px-1 leading-none text-ink-soft hover:text-ink"
          onClick={() => {
            rememberHidden(scope, status.state);
            setHidden(status.state);
          }}
        >
          <IconX size={12} />
        </button>
      )}
    </div>
  );
}
