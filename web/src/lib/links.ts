import type { LinkStatus } from "@/lib/ipc";

/// The forges a link can name, with the host each uses by default. Self-managed
/// GitLab, Forgejo, Gitea and GitHub Enterprise are added on this device under
/// Linked accounts (HTTPS only).
export const FORGES: { provider: string; host: string; label: string; keyPublished: boolean; hint: string }[] = [
  { provider: "github", host: "github.com", label: "GitHub", keyPublished: true, hint: "your username" },
  { provider: "gitlab", host: "gitlab.com", label: "GitLab", keyPublished: true, hint: "your username" },
  { provider: "forgejo", host: "codeberg.org", label: "Codeberg", keyPublished: false, hint: "your username" },
  { provider: "gitea", host: "gitea.com", label: "Gitea", keyPublished: false, hint: "your username" },
];

/// Forges whose verifiers 0.1 dropped: links made by a 0.1 pre-release still
/// show (self-asserted), under their name, but none is offered for a new link.
const DROPPED_FORGES: { provider: string; host: string; label: string }[] = [
  { provider: "sourcehut", host: "meta.sr.ht", label: "SourceHut" },
  { provider: "tangled", host: "atproto", label: "Tangled" },
];

export function forgeLabel(provider: string, host: string): string {
  const all = [...FORGES, ...DROPPED_FORGES];
  const known = all.find((f) => f.provider === provider && f.host === host);
  if (known) return known.label;
  const p = all.find((f) => f.provider === provider)?.label ?? provider;
  return `${p} (${host})`;
}

/// A name without control, bidi or zero-width characters (they can reorder
/// the text after them or make one name look like another). The same set
/// as `hive_proto::names::is_hidden_name_char`, supplementary planes
/// included (musical format characters, tags and the variation selectors
/// supplement): checked against the shared display-name vectors.
export function stripBidi(s: string): string {
  // eslint-disable-next-line no-control-regex, no-misleading-character-class
  return s.replace(/[\u0000-\u001F\u007F-\u009F\u00AD\u034F\u061C\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B-\u200F\u202A-\u202E\u2060-\u206F\u3164\uFE00-\uFE0F\uFEFF\uFFA0\uFFF9-\uFFFB\u{1D173}-\u{1D17A}\u{E0000}-\u{E0FFF}]/gu, "");
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
        label: "verified",
        title: "The forge lists the key that signed this link (checked by this device).",
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
      return { kind: "claim", label: "self-asserted", title: "A claim only: Ligus doesn't check this forge's keys." };
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

/// What a person types, for the forge: a handle never starts with "@".
export function normalizeHandle(handle: string): string {
  return handle.trim().replace(/^@/, "");
}

/// The shell lines that sign each statement with the user's own key, one
/// file per workspace (`ssh-keygen` prints the signature to stdout).
export function signScript(namespace: string, keyPath: string, statements: { workspaceName: string }[]): string {
  return statements
    .map((s, i) => `ssh-keygen -Y sign -n ${namespace} -f ${keyPath} < statement-${i + 1}.txt  # ${s.workspaceName}`)
    .join("\n");
}
