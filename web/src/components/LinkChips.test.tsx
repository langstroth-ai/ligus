import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { LinkChips } from "./LinkChips";
import { FORGES, chipFor, forgeLabel, normalizeHandle, signScript } from "@/lib/links";
import { CONTACT_CARD_LINK_DISCLOSURE, KEY_PUBLISHED_DISCLOSURE } from "@/components/settings/LinkedAccountsSection";
import type { LinkChipDto } from "@/lib/ipc";

const base = { provider: "github", host: "github.com", handle: "alice", subjectId: "1234567" };

describe("chipFor", () => {
  it("only a forge check verifies; a self-asserted link never does", () => {
    expect(chipFor({ state: "verified", checkedAt: "x", handle: "a" })?.kind).toBe("verified");
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
  /// 0.1 dropped the SourceHut and Tangled verifiers: a link of either kind
  /// made by a 0.1 pre-release arrives self-asserted and is a claim, never
  /// verified; neither forge is offered for a new link.
  it("SourceHut and Tangled links are claims only, and not offered", () => {
    expect(chipFor({ state: "selfAsserted" })?.label).toBe("self-asserted");
    expect(chipFor({ state: "selfAsserted" })?.title).not.toMatch(/verified/);
    expect(FORGES.map((f) => f.provider)).not.toContain("sourcehut");
    expect(FORGES.map((f) => f.provider)).not.toContain("tangled");
    expect(forgeLabel("sourcehut", "meta.sr.ht")).toBe("SourceHut");
    expect(forgeLabel("tangled", "atproto")).toBe("Tangled");
  });
});

describe("LinkChips", () => {
  /// S10 `no_remote_avatar_urls_rendered`: chips are text; nothing is loaded
  /// from a forge (no image, no link that would be fetched or followed).
  it("renders no remote avatar or URL", () => {
    const links: LinkChipDto[] = [
      { ...base, status: { state: "verified", checkedAt: "x", handle: "alice2" } },
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
  /// The handle in a link is its publisher's own claim. A verified chip
  /// shows only a handle the forge confirmed; when the forge didn't say
  /// (GitLab's anonymous API), it shows the stable id, never the claimed handle.
  it("never shows a self-claimed handle as verified", () => {
    const links: LinkChipDto[] = [
      { ...base, provider: "gitlab", host: "gitlab.com", handle: "torvalds", subjectId: "999", status: { state: "verified", checkedAt: "x", handle: null } },
    ];
    const { container } = render(<LinkChips links={links} />);
    const chip = container.querySelector('[data-chip="verified"]')!;
    expect(chip.textContent).not.toMatch(/torvalds/);
    expect(chip.textContent).toMatch(/id 999/);
  });
  /// Bidi overrides and zero-width characters in a handle can't reorder the
  /// chip's label or hide in a look-alike name; the handle is isolated.
  it("strips bidi and zero-width characters and isolates the handle", () => {
    const links: LinkChipDto[] = [
      { ...base, handle: "ali\u200Bce\u202E", status: { state: "selfAsserted" } },
    ];
    const { container } = render(<LinkChips links={links} />);
    expect(container.textContent).not.toMatch(/[\u200B-\u200F\u202A-\u202E\u2066-\u2069\uFEFF]/);
    expect(container.querySelector("bdi")?.textContent).toBe("alice");
  });
  it("shows nothing for invalid links only", () => {
    const { container } = render(<LinkChips links={[{ ...base, status: { state: "invalid", reason: "x" } }]} />);
    expect(container.innerHTML).toBe("");
  });
});

describe("linking helpers", () => {
  it("normalizes handles", () => {
    expect(normalizeHandle(" @alice ")).toBe("alice");
  });
  it("signs under the link namespace only", () => {
    const s = signScript("ligus-identity-link-v1@ligus.ai", "~/.ssh/id_ed25519", [{ workspaceName: "Bees" }]);
    expect(s).toContain("ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai -f ~/.ssh/id_ed25519");
  });
  /// The contact card is a plain signed record on the relay: the opt-in says
  /// who can read a link put on it.
  it("the contact-card opt-in says the relay and contact-id holders read it", () => {
    expect(CONTACT_CARD_LINK_DISCLOSURE).toMatch(/relay/);
    expect(CONTACT_CARD_LINK_DISCLOSURE).toMatch(/isn't encrypted/);
  });
  it("the published-key fallback says it is public", () => {
    expect(KEY_PUBLISHED_DISCLOSURE).toMatch(/publicly/);
    expect(KEY_PUBLISHED_DISCLOSURE).toMatch(/no access/);
  });
});
