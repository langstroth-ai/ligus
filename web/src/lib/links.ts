import type { LinkStatus } from "@/lib/ipc";

/// The forges a link can name, with the host each uses by default. Self-managed
/// GitLab, Forgejo, Gitea and GitHub Enterprise are added on this device under
/// Linked accounts (HTTPS only).
export const FORGES: { provider: string; host: string; label: string; keyPublished: boolean; hint: string }[] = [
  { provider: "github", host: "github.com", label: "GitHub", keyPublished: true, hint: "your username" },
  { provider: "gitlab", host: "gitlab.com", label: "GitLab", keyPublished: true, hint: "your username" },
  { provider: "forgejo", host: "codeberg.org", label: "Codeberg", keyPublished: false, hint: "your username" },
  { provider: "gitea", host: "gitea.com", label: "Gitea", keyPublished: false, hint: "your username" },
  { provider: "sourcehut", host: "meta.sr.ht", label: "SourceHut", keyPublished: false, hint: "~username" },
  { provider: "tangled", host: "atproto", label: "Tangled", keyPublished: false, hint: "your handle or DID" },
];

export function forgeLabel(provider: string, host: string): string {
  const known = FORGES.find((f) => f.provider === provider && f.host === host);
  if (known) return known.label;
  const p = FORGES.find((f) => f.provider === provider)?.label ?? provider;
  return `${p} (${host})`;
}

/// A name without control, bidi or zero-width characters (they can reorder
/// the text after them or make one name look like another).
export function stripBidi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return s.replace(/[\u0000-\u001F\u007F-\u009F\u00AD\u034F\u061C\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B-\u200F\u202A-\u202E\u2060-\u206F\u3164\uFE00-\uFE0F\uFEFF\uFFA0\uFFF9-\uFFFB]/g, "");
}

export type ChipKind = "verified" | "unverified" | "stale" | "claim";

/// How a link is shown. Only a forge check makes a link "verified"; a
/// self-asserted one is a claim, never verified; one that failed the offline
/// checks isn't shown at all (`null`).
export function chipFor(status: LinkStatus | null | undefined): { kind: ChipKind; label: string; title: string } | null {
  if (!status) return { kind: "unverified", label: "not checked", title: "Not checked against the forge yet." };
  switch (status.state) {
    case "verified":
      return {
        kind: "verified",
        label: status.nameBound ? "verified by name" : "verified",
        title: status.nameBound
          ? "The forge lists the signing key for this name. SourceHut has no stable id, so a reused name could match."
          : "The forge lists the key that signed this link (checked by this device).",
      };
    case "stale":
      return { kind: "stale", label: "stale", title: status.reason };
    case "expired":
      return { kind: "stale", label: "expired", title: "This link expired; its owner can re-sign it." };
    case "unreachable":
      return {
        kind: "unverified",
        label: status.lastVerifiedAt ? "unverified (forge unreachable)" : "unverified",
        title: status.reason,
      };
    case "signatureOnly":
      return {
        kind: "unverified",
        label: "signature valid",
        title: "Signature valid, not checked against the forge (forge checks are off on this device).",
      };
    case "notChecked":
      return { kind: "unverified", label: "not checked", title: status.reason };
    case "selfAsserted":
      return { kind: "claim", label: "self-asserted", title: "A claim only: this forge has no public key list." };
    case "invalid":
      return null;
  }
}

/// The colors of a chip (tokens only).
export function chipColor(kind: ChipKind): { border: string; ink: string } {
  switch (kind) {
    case "verified":
      return { border: "var(--hive-accent-cool)", ink: "var(--hive-ink)" };
    case "stale":
      return { border: "var(--hive-danger-ink)", ink: "var(--hive-danger-ink)" };
    default:
      return { border: "var(--hive-line)", ink: "var(--hive-ink-soft)" };
  }
}

/// What a person types, for the forge: a SourceHut name always starts with
/// "~"; other handles never start with "@".
export function normalizeHandle(provider: string, handle: string): string {
  const h = handle.trim().replace(/^@/, "");
  if (provider === "sourcehut") return h.startsWith("~") ? h : `~${h}`;
  return h;
}

/// The shell lines that sign each statement with the user's own key, one
/// file per workspace (`ssh-keygen` prints the signature to stdout).
export function signScript(namespace: string, keyPath: string, statements: { workspaceName: string }[]): string {
  return statements
    .map((s, i) => `ssh-keygen -Y sign -n ${namespace} -f ${keyPath} < statement-${i + 1}.txt  # ${s.workspaceName}`)
    .join("\n");
}
