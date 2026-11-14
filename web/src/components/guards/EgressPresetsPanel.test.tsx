import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { EgressPresetsDto } from "@/bindings/EgressPresetsDto";

const state: { d: EgressPresetsDto } = { d: null as unknown as EgressPresetsDto };
const setWorkspace = vi.fn(async (_p: string[]) => {});
const setChat = vi.fn(async (_id: string, _p: string[] | null) => {});
vi.mock("@/lib/ipc", () => ({
  getEgressPresets: vi.fn(async () => state.d),
  setWorkspaceEgressPresets: (p: string[]) => setWorkspace(p),
  setChatEgressPresets: (id: string, p: string[] | null) => setChat(id, p),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { EgressPresetsPanel, chatChoice, nextChatPresets } from "./EgressPresetsPanel";

const base: EgressPresetsDto = {
  available: [
    { name: "package-registries", label: "Package registries", hosts: ["registry.npmjs.org", "crates.io"] },
    { name: "claude-connectors", label: "Claude connectors", hosts: ["mcp-proxy.anthropic.com"] },
  ],
  workspace: [],
  chat: null,
  host: [],
  restrictedBy: "",
};

function renderPanel(chat: { id: string; title: string } | null = null) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <EgressPresetsPanel chat={chat} />
    </QueryClientProvider>,
  );
}

describe("EgressPresetsPanel", () => {
  beforeEach(() => {
    state.d = { ...base };
    setWorkspace.mockClear();
    setChat.mockClear();
  });

  it("shows every preset off by default and turns one on for the workspace", async () => {
    renderPanel();
    const sw = await screen.findByRole("switch", { name: "Package registries for this workspace" });
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("switch", { name: "Claude connectors for this workspace" }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText(/reach every service you connected/)).toBeTruthy();
    await userEvent.click(sw);
    await waitFor(() => expect(setWorkspace).toHaveBeenCalledWith(["package-registries"]));
  });

  it("offers a per-chat override and says when a guard overrides it", async () => {
    state.d = { ...base, workspace: ["package-registries"], restrictedBy: "lockdown" };
    renderPanel({ id: "c1", title: "Build" });
    const sel = await screen.findByRole("combobox", { name: "Package registries for this chat" });
    expect(screen.getAllByText("ignored: chat locked down").length).toBe(2);
    await userEvent.selectOptions(sel, "off");
    await waitFor(() => expect(setChat).toHaveBeenCalledWith("c1", []));
  });

  it("computes chat overrides", () => {
    const d = { ...base, workspace: ["package-registries"] };
    expect(chatChoice(d, "package-registries")).toBe("inherit");
    expect(nextChatPresets(d, "claude-connectors", "on")).toEqual(["package-registries", "claude-connectors"]);
    expect(nextChatPresets(d, "package-registries", "off")).toEqual([]);
    const overridden = { ...d, chat: [] as string[] };
    expect(chatChoice(overridden, "package-registries")).toBe("off");
    expect(nextChatPresets(overridden, "package-registries", "inherit")).toBeNull();
  });
});
