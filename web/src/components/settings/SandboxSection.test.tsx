import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { SandboxGrants, SandboxPolicyDto } from "@/lib/ipc";

const state: { d: SandboxPolicyDto } = { d: null as unknown as SandboxPolicyDto };
const setGrants = vi.fn(async (_g: SandboxGrants) => {});
const setHere = vi.fn(async (_a: boolean | null) => {});
const ack = vi.fn(async () => {});
vi.mock("@/lib/ipc", () => ({
  getSandboxPolicy: vi.fn(async () => state.d),
  setSandboxGrants: (g: SandboxGrants) => setGrants(g),
  setSandboxGrantsHere: (a: boolean | null) => setHere(a),
  ackSandboxGrants: () => ack(),
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
      localServers: "off: no listening sockets",
    },
  ],
  grants: { workspace: { readOnly: ["~/data"] } },
  agents: [{ id: AGENT, name: "coder", runtime: "ClaudeCode" }],
  canEditPaths: true,
  canEditShell: true,
  local: false,
  appliesHere: true,
  appliesByDefault: true,
  appliesSetting: null,
  appliesWhy: "applied: this device is the workspace Owner's (the default)",
  changed: false,
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
    setHere.mockClear();
    ack.mockClear();
  });

  it("lets a member's device opt into the grants, and back to the default", async () => {
    state.d = { ...base, appliesHere: false, appliesByDefault: false, appliesWhy: "not applied: this device isn't the workspace Owner's" };
    renderSection();
    const sw = await screen.findByRole("switch", { name: "Apply workspace sandbox grants on this device" });
    expect(screen.getByText(/this device isn't the workspace Owner's/)).toBeTruthy();
    await userEvent.click(sw);
    await waitFor(() => expect(setHere).toHaveBeenCalledWith(true));
  });

  it("turning an opted-in device back to its default clears the setting", async () => {
    state.d = { ...base, appliesHere: true, appliesByDefault: false, appliesSetting: true };
    renderSection();
    await userEvent.click(await screen.findByRole("switch", { name: "Apply workspace sandbox grants on this device" }));
    await waitFor(() => expect(setHere).toHaveBeenCalledWith(null));
  });

  it("says when the grants changed until you've seen them", async () => {
    state.d = { ...base, changed: true };
    renderSection();
    expect(await screen.findByText("These grants changed since you last looked.")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Got it" }));
    await waitFor(() => expect(ack).toHaveBeenCalled());
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

  it("lets the Owner turn on local servers for one agent, off by default", async () => {
    renderSection();
    await userEvent.selectOptions(await screen.findByLabelText("Show the policy for"), AGENT);
    const sw = await screen.findByRole("switch", { name: "May run local servers (@coder)" });
    expect(sw.getAttribute("aria-checked")).toBe("false");
    await userEvent.click(sw);
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setGrants).toHaveBeenCalledWith({ workspace: { readOnly: ["~/data"] }, agents: { [AGENT]: { localServers: true } } }),
    );
    expect(screen.getByText(/so it has no effect on this Mac/)).toBeTruthy();
  });

  it("an Admin who isn't the Owner can't change local servers", async () => {
    state.d = { ...base, canEditShell: false, grants: { agents: { [AGENT]: { localServers: true } } } };
    renderSection();
    await userEvent.selectOptions(await screen.findByLabelText("Show the policy for"), AGENT);
    const sw = await screen.findByRole("switch", { name: "May run local servers (@coder)" });
    expect((sw as HTMLButtonElement).disabled).toBe(true);
    expect(sw.getAttribute("aria-checked")).toBe("true");
  });

  it("an agent row's Sandbox… opens the section on that agent", async () => {
    const nav = await import("@/lib/nav");
    nav.openSandboxForAgent(AGENT);
    renderSection();
    expect(await screen.findByRole("switch", { name: "May run local servers (@coder)" })).toBeTruthy();
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
