import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { WorkspaceRulesSection } from "./WorkspaceRulesSection";
import * as ipc from "@/lib/ipc";
import * as dialog from "@/components/Dialog";
import type { Rules4StatusDto } from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  workspaceRules4Status: vi.fn(),
  workspaceRulesUpgrade: vi.fn(async () => true),
  setupRun: vi.fn(async () => ({ exitCode: 0, checks: [] })),
}));
vi.mock("@/components/Dialog", () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() }, errMsg: (e: unknown) => String(e) }));

const base: Rules4StatusDto = {
  rules: 3,
  isOwner: true,
  changes: ["Each agent carries its own model choice.", "There is no going back to the older rules."],
  readiness: ["lt on zaz-mbp (Ana) is on 0.4.1: it goes read-only here until it's updated"],
  oldDevices: [{ name: "lt on zaz-mbp", person: "Ana", build: "0.4.1" }],
  unknownDevices: [],
  endpointRuntimes: [],
  pins: [],
};

function renderIt() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <WorkspaceRulesSection />
    </QueryClientProvider>,
  );
}

describe("WorkspaceRulesSection", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows an Owner the readiness list, names the old device, and upgrades with one confirm", async () => {
    vi.mocked(ipc.workspaceRules4Status).mockResolvedValue(base);
    renderIt();
    expect((await screen.findByTestId("rules4-readiness")).textContent).toContain("lt on zaz-mbp (Ana) is on 0.4.1");
    // What changes stays behind its disclosure until asked for.
    expect(screen.queryByTestId("rules4-changes")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "What changes" }));
    expect(screen.getByTestId("rules4-changes").textContent).toContain("no going back");
    await userEvent.click(screen.getByRole("button", { name: "Upgrade workspace rules" }));
    expect(vi.mocked(dialog.confirmDialog).mock.calls[0][0]).toContain("lt on zaz-mbp goes read-only here");
    await waitFor(() => expect(ipc.workspaceRulesUpgrade).toHaveBeenCalledTimes(1));
  });

  it("says everything is ready when nothing changes for anyone", async () => {
    vi.mocked(ipc.workspaceRules4Status).mockResolvedValue({ ...base, readiness: [], oldDevices: [] });
    renderIt();
    expect(await screen.findByTestId("rules4-ready")).toHaveTextContent("Every device here is ready.");
  });

  it("offers no switch to someone who isn't the Owner", async () => {
    vi.mocked(ipc.workspaceRules4Status).mockResolvedValue({ ...base, isOwner: false });
    renderIt();
    expect(await screen.findByTestId("rules4-not-owner")).toHaveTextContent("Its Owner can upgrade it.");
    expect(screen.queryByRole("button", { name: "Upgrade workspace rules" })).toBeNull();
  });

  it("on rules 4, pins an agent that still names a runtime through the agent flow", async () => {
    vi.mocked(ipc.workspaceRules4Status).mockResolvedValue({
      ...base,
      rules: 4,
      readiness: [],
      pins: [{ agentId: "a1", name: "scout", label: "Pin @scout to Claude Code / opus", mine: true }],
    });
    renderIt();
    expect(await screen.findByTestId("rules4-on")).toBeInTheDocument();
    expect(screen.getByText("Pin @scout to Claude Code / opus")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Pin" }));
    await waitFor(() => expect(ipc.setupRun).toHaveBeenCalledWith("agent", { agent: "a1" }, {}));
    expect(screen.queryByRole("button", { name: "Upgrade workspace rules" })).toBeNull();
  });
});
