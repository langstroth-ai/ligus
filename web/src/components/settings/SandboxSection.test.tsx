import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { SandboxGrants, SandboxPolicyDto } from "@/lib/ipc";

const state: { d: SandboxPolicyDto } = { d: null as unknown as SandboxPolicyDto };
const setGrants = vi.fn(async (_g: SandboxGrants) => {});
vi.mock("@/lib/ipc", () => ({
  getSandboxPolicy: vi.fn(async () => state.d),
  setSandboxGrants: (g: SandboxGrants) => setGrants(g),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { SandboxSection, normalizeGrants, parsePaths, pathLooksOk } from "./SandboxSection";

const AGENT = "00000000-0000-4000-8000-0000000000a1";

const base: SandboxPolicyDto = {
  policies: [
    {
      runtime: "ClaudeCode",
      tier: "seatbelt",
      home: "/Users/u",
      entries: [
        { path: "/Users/u/.cargo/bin", access: "read", why: "toolchain: rustup proxies" },
        { path: "/Users/u", access: "denied", why: "your home dir: everything not listed above" },
      ],
      shell: "the workspace Owner's turns only",
    },
  ],
  grants: { workspace: { readOnly: ["~/data"] } },
  agents: [{ id: AGENT, name: "coder", runtime: "ClaudeCode" }],
  canEditPaths: true,
  canEditShell: true,
  local: false,
};

function renderSection() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <SandboxSection />
    </QueryClientProvider>,
  );
}

describe("SandboxSection", () => {
  beforeEach(() => {
    state.d = { ...base };
    setGrants.mockClear();
  });

  it("lists each path with its access and reason, and the shell rule", async () => {
    renderSection();
    expect(await screen.findByText("/Users/u/.cargo/bin")).toBeTruthy();
    expect(screen.getByText("toolchain: rustup proxies")).toBeTruthy();
    expect(screen.getByText("Denied")).toBeTruthy();
    expect(screen.getByText(/the workspace Owner's turns only/)).toBeTruthy();
    expect((screen.getByLabelText("Every agent: read-only") as HTMLTextAreaElement).value).toBe("~/data");
  });

  it("disables the shell toggle for a non-Owner", async () => {
    state.d = { ...base, canEditShell: false };
    renderSection();
    await userEvent.selectOptions(await screen.findByLabelText("Show the policy for"), AGENT);
    const sw = await screen.findByRole("switch", { name: "Keep the shell for other members' turns (@coder)" });
    expect((sw as HTMLButtonElement).disabled).toBe(true);
  });

  it("saves the edited set: workspace paths, an agent's paths and its shell", async () => {
    renderSection();
    const rw = await screen.findByLabelText("Every agent: read and write");
    await userEvent.type(rw, "~/scratch");
    await userEvent.selectOptions(screen.getByLabelText("Show the policy for"), AGENT);
    await userEvent.type(await screen.findByLabelText("@coder: read-only"), "/opt/models");
    await userEvent.click(screen.getByRole("switch", { name: "Keep the shell for other members' turns (@coder)" }));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setGrants).toHaveBeenCalledWith({
        workspace: { readOnly: ["~/data"], readWrite: ["~/scratch"] },
        agents: { [AGENT]: { readOnly: ["/opt/models"], shell: true } },
      }),
    );
  });

  it("keeps Save off for a member below Admin", async () => {
    state.d = { ...base, canEditPaths: false, canEditShell: false };
    renderSection();
    const ro = await screen.findByLabelText("Every agent: read-only");
    expect((ro as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("grant helpers", () => {
  it("parses, checks and normalizes paths", () => {
    expect(parsePaths(" ~/a \n\n/b\n")).toEqual(["~/a", "/b"]);
    expect(["~/a", "/b"].every(pathLooksOk)).toBe(true);
    expect(["~", "rel", "/"].some(pathLooksOk)).toBe(false);
    expect(normalizeGrants({ workspace: { readOnly: [], readWrite: [] }, agents: { x: { readOnly: [], shell: false } } })).toEqual({});
  });
});
