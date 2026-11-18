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
