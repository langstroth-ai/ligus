import { describe, expect, it } from "vitest";
import { authLabel, FORGE_KINDS } from "./CodeHostsSection";
import type { ForgeConnection } from "@/lib/ipc";

const base: ForgeConnection = {
  id: "gitea:codeberg.org",
  kind: "gitea",
  displayName: "Codeberg",
  baseUrl: "https://codeberg.org",
  authSource: "token",
  hasRead: true,
  hasWrite: false,
  deviceFlow: false,
  openLabel: "Open WIP PR",
  holder: "this device",
};

describe("Code hosts", () => {
  it("names where a credential comes from, never the token", () => {
    expect(authLabel(base)).toBe("token (read only)");
    expect(authLabel({ ...base, hasWrite: true })).toBe("token (read and write)");
    expect(authLabel({ ...base, authSource: "gh" })).toMatch(/gh/);
    expect(authLabel({ ...base, authSource: "none" })).toMatch(/public/);
  });
  it("offers GitHub, GitLab, Codeberg, Forgejo and Gitea, all over https", () => {
    expect(FORGE_KINDS.map((k) => k.kind)).toEqual(["github", "gitlab", "codeberg", "forgejo", "gitea"]);
    for (const k of FORGE_KINDS) expect(k.base.startsWith("https://")).toBe(true);
  });
});
