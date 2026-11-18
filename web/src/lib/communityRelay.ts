/// The community relay: a free, best-effort relay run by the Langstroth
/// project. It is only ever a preset in the relay pickers (onboarding, a new
/// workspace, Settings → Team sync) that the person chooses; Ligus never falls
/// back to it silently. Mirrored in Rust as `hive_core::COMMUNITY_RELAY_URL`
/// (a test there checks the two match).
export const COMMUNITY_RELAY_URL = "https://relay.langstroth.ai";

/// How the pickers name it.
export const COMMUNITY_RELAY_LABEL = "Community relay (relay.langstroth.ai, best-effort)";

/// Where the picker explains what "best-effort" means.
export const COMMUNITY_RELAY_DOCS = "https://docs.langstroth.ai/operate/networking/community-relay/";

/// A relay choice in a picker: the community relay, a URL the person types,
/// or no relay at all (this device only).
export type RelayChoice = "community" | "own" | "none";

/// The URL a picker choice resolves to ("" for no relay).
export function relayUrlFor(choice: RelayChoice, ownUrl: string): string {
  if (choice === "community") return COMMUNITY_RELAY_URL;
  if (choice === "own") return ownUrl.trim();
  return "";
}
