// "Is this you on another computer?": a member of the workspace that is very
// likely the person at this desktop, joined a second time from another
// computer (lt or the `ligus` CLI redeemed an invite instead of being added
// under My devices). Only a hint: it never acts, it only offers Add a
// computer. Computed here from what the desktop already has (the roster and
// the host list); nothing is sent anywhere.
//
// Conservative on purpose, so a teammate who happens to share a first name
// isn't asked about:
//   - a current human member other than you, not the founder (the founder
//     came first; a later self-invite can't be it);
//   - and either the same display name (ignoring case, spaces and
//     punctuation), or one name a prefix of the other (at least 3
//     characters: "zaz" / "zazulam") while that member also registered a
//     terminal or CLI host here (an lt or `ligus` device label).

import type { WorkspaceHostDto, WorkspaceMemberDto } from "@/lib/ipc";

/// Lowercase letters and digits only ("Mike Z." → "mikez").
export function nameKey(name: string): string {
  return name.toLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
}

/// A host label lt or the `ligus` CLI gives a computer ("lt on Michaels-Air-3").
export function isTerminalHostLabel(label: string): boolean {
  return /^(lt|ligus)\b/i.test(label.trim());
}

export type LikelyMeReason = "same-name" | "name-and-terminal";

export function likelyMeReason(
  member: Pick<WorkspaceMemberDto, "displayName" | "kind" | "isSelf" | "isFounder">,
  myName: string,
  hosts: Pick<WorkspaceHostDto, "label" | "ownerName" | "mine">[],
): LikelyMeReason | null {
  if (member.isSelf || member.isFounder || member.kind !== "human") return null;
  const a = nameKey(member.displayName);
  const b = nameKey(myName);
  if (!a || !b) return null;
  if (a === b) return "same-name";
  const [short, long] = a.length <= b.length ? [a, b] : [b, a];
  if (short.length < 3 || !long.startsWith(short)) return null;
  const terminal = hosts.some(
    (h) => !h.mine && h.ownerName === member.displayName && isTerminalHostLabel(h.label),
  );
  return terminal ? "name-and-terminal" : null;
}

export const LIKELY_ME_HINT = "Is this you on another computer? Link it to your devices";
