import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { LinkChips } from "./LinkChips";
import { chipFor, normalizeHandle, signScript } from "@/lib/links";
import { KEY_PUBLISHED_DISCLOSURE } from "@/components/settings/LinkedAccountsSection";
import type { LinkChipDto } from "@/lib/ipc";

const base = { provider: "github", host: "github.com", handle: "alice", subjectId: "1234567" };

describe("chipFor", () => {
  it("only a forge check verifies; a self-asserted link never does", () => {
    expect(chipFor({ state: "verified", checkedAt: "x", handle: "a", nameBound: false })?.kind).toBe("verified");
    expect(chipFor({ state: "selfAsserted" })?.kind).toBe("claim");
    expect(chipFor({ state: "signatureOnly" })?.kind).toBe("unverified");
    expect(chipFor({ state: "unreachable", lastVerifiedAt: "x", reason: "r" })?.kind).toBe("unverified");
    expect(chipFor(null)?.kind).toBe("unverified");
    expect(chipFor({ state: "stale", checkedAt: "x", reason: "key removed" })?.kind).toBe("stale");
    expect(chipFor({ state: "expired" })?.kind).toBe("stale");
  });
  it("hides a link that failed the offline checks", () => {
    expect(chipFor({ state: "invalid", reason: "bad signature" })).toBeNull();
  });
  it("marks SourceHut links as bound to a name", () => {
    expect(chipFor({ state: "verified", checkedAt: "x", handle: "~d", nameBound: true })?.label).toMatch(/by name/);
  });
});

describe("LinkChips", () => {
  /// S10 `no_remote_avatar_urls_rendered`: chips are text; nothing is loaded
  /// from a forge (no image, no link that would be fetched or followed).
  it("renders no remote avatar or URL", () => {
    const links: LinkChipDto[] = [
      { ...base, status: { state: "verified", checkedAt: "x", handle: "alice2", nameBound: false } },
      { ...base, provider: "forgejo", host: "codeberg.org", status: null },
    ];
    const { container } = render(<LinkChips links={links} />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("a")).toBeNull();
    expect(container.innerHTML).not.toMatch(/https?:/);
    // The current handle from the forge, and the stable id beside it.
    expect(screen.getByText("alice2")).toBeTruthy();
    expect(container.querySelector('[data-chip="verified"]')?.getAttribute("title")).toMatch(/id 1234567/);
    expect(container.querySelector('[data-chip="unverified"]')).toBeTruthy();
  });
  it("shows nothing for invalid links only", () => {
    const { container } = render(<LinkChips links={[{ ...base, status: { state: "invalid", reason: "x" } }]} />);
    expect(container.innerHTML).toBe("");
  });
});

describe("linking helpers", () => {
  it("normalizes handles per forge", () => {
    expect(normalizeHandle("github", " @alice ")).toBe("alice");
    expect(normalizeHandle("sourcehut", "dave")).toBe("~dave");
    expect(normalizeHandle("sourcehut", "~dave")).toBe("~dave");
  });
  it("signs under the link namespace only", () => {
    const s = signScript("ligus-identity-link-v1@ligus.ai", "~/.ssh/id_ed25519", [{ workspaceName: "Bees" }]);
    expect(s).toContain("ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai -f ~/.ssh/id_ed25519");
  });
  it("the published-key fallback says it is public", () => {
    expect(KEY_PUBLISHED_DISCLOSURE).toMatch(/publicly/);
    expect(KEY_PUBLISHED_DISCLOSURE).toMatch(/no access/);
  });
});
