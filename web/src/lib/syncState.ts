import type { SyncStatusDto } from "@/lib/ipc";

/// A short label for a `limited` sync (see `ConnectionState`), from the
/// backend's message. The message is the contract's first words:
/// "workspace full", "organisation quota reached", "room expired or
/// truncated", "the relay is rate limiting", "the relay is full".
export function limitedLabel(msg: string | null): string {
  if (!msg) return "Sync limited";
  if (msg.startsWith("workspace full")) return "Workspace full";
  if (msg.startsWith("organisation quota")) return "Organisation quota reached";
  if (msg.startsWith("room expired or truncated")) return "Room expired or truncated";
  if (msg.includes("rate limiting")) return "Sync slowed by the relay";
  if (msg.startsWith("the relay is full")) return "Relay full";
  return "Sync limited";
}

/// The sync state in a few words, driven by real connection health (not just
/// "a relay URL is set"): live → success, error → danger (with the failure in
/// the title), limited or a missing key → warn, offline → neutral.
export type SyncPill = { label: string; color: "success" | "danger" | "warn" | null; title?: string; actionable?: boolean };

export function deriveSyncPill(s: SyncStatusDto | undefined): SyncPill {
  const state = s?.connectionState ?? "offline";
  if (state === "live") {
    return {
      label: `Live · room ${s?.room ?? ""}`,
      color: "success",
      title: "Live — syncing with your team",
    };
  }
  // Syncing, but the relay refused this workspace's writes (workspace full,
  // an organisation quota) or the room is truncated: amber, with the reason.
  if (state === "limited") {
    return {
      label: limitedLabel(s?.lastError ?? null),
      color: "warn",
      title: s?.lastError ?? "The relay isn't taking new writes for this workspace right now",
      actionable: true,
    };
  }
  if (state === "error") {
    return {
      label: "Sync error",
      color: "danger",
      title: s?.lastError ?? "Sync failed — reconnect or check your key / token",
      actionable: true,
    };
  }
  // Relay configured but no E2EE key yet — setup-incomplete, not a failure.
  // Calm amber prompt to set a passphrase (Settings → Team), never a red error.
  if (state === "needs_key") {
    return {
      label: "Set a passphrase to sync",
      color: "warn",
      title:
        s?.lastError ??
        "A relay is configured but this workspace has no encryption key — set a workspace passphrase in Settings → Team to sync encrypted, or clear the relay URL to work local-only.",
      actionable: true,
    };
  }
  return {
    label: s?.relayConfigured ? "Offline · not syncing" : "Local only",
    color: null,
    title: s?.relayConfigured
      ? "A relay is configured but nothing is syncing right now"
      : undefined,
  };
}
