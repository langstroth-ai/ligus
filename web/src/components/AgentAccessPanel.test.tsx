import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { WorkspaceAgentDto } from "@/lib/ipc";
import type { AgentUsageDto } from "@/bindings/AgentUsageDto";

const setAccess = vi.fn(async (..._a: unknown[]) => {});
const usage: { d: AgentUsageDto } = { d: null as unknown as AgentUsageDto };
vi.mock("@/lib/ipc", () => ({
  setAgentAccess: (...a: unknown[]) => setAccess(...a),
  getAgentUsage: vi.fn(async () => usage.d),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { AgentAccessPanel, accessLabel, accessSummary, parseUsdCents } from "./AgentAccessPanel";

const base: WorkspaceAgentDto = {
  id: "ag-1",
  name: "scout",
  runtimeId: "rt",
  role: "",
  ownerActorId: "me",
  hostId: "",
  avatarUrl: null,
  avatarColorHex: null,
  access: "owner",
  memberTurnsPerDay: null,
  effectiveMemberTurnsPerDay: 50,
  usdCentsPerDay: null,
  notifyOnUse: false,
  available: true,
  unavailableReason: null,
  canManageAccess: true,
};

/// Renders the panel and opens its folded "Access" editor (when it has one).
function renderPanel(agent: WorkspaceAgentDto, opened = true) {
  const r = renderClosed(agent);
  const toggle = screen.queryByRole("button", { name: /^Access/ });
  if (opened && toggle) fireEvent.click(toggle);
  return r;
}

function renderClosed(agent: WorkspaceAgentDto) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <AgentAccessPanel sessionId="s1" agent={agent} />
    </QueryClientProvider>,
  );
}

describe("AgentAccessPanel", () => {
  beforeEach(() => {
    setAccess.mockClear();
    usage.d = {
      agentId: "ag-1",
      day: "2026-11-20",
      today: [{ member: "mate", memberName: "Mate", turns: 2, uncostedTurns: 0, estimatedCostUsd: 0.1 }],
      month: [
        { member: "mate", memberName: "Mate", turns: 7, uncostedTurns: 0, estimatedCostUsd: 0.75 },
        { member: "cli", memberName: "Cli", turns: 3, uncostedTurns: 3, estimatedCostUsd: null },
      ],
    };
  });

  it("the owner opens the agent to everyone with budgets and the notice", async () => {
    const user = userEvent.setup();
    renderPanel(base);
    expect(screen.getByRole("option", { name: "Only me" })).toBeInTheDocument();
    // The explanations sit behind "?" buttons but stay in the accessibility tree.
    expect(screen.getByRole("button", { name: "About who can use this agent" })).toHaveAccessibleDescription(
      /on their credentials/,
    );
    expect(screen.getByRole("button", { name: "About the daily cap" })).toHaveAccessibleDescription(/never limited/);
    await user.selectOptions(screen.getByLabelText("Who can use this agent"), "everyone");
    await user.selectOptions(screen.getByLabelText("Turns per member per day"), "custom");
    await user.type(screen.getByLabelText("Turns per member per day (number)"), "20");
    await user.type(screen.getByLabelText("Daily spending cap in US dollars"), "2.50");
    await user.click(screen.getByLabelText("Notify me when someone else uses this agent"));
    await user.click(screen.getByRole("button", { name: "Save access" }));
    await waitFor(() => expect(setAccess).toHaveBeenCalledWith("ag-1", "everyone", 20, 250, true));
  });

  it("unlimited is 0 and the default is null", async () => {
    const user = userEvent.setup();
    renderPanel({ ...base, access: "everyone", memberTurnsPerDay: 10 });
    await user.selectOptions(screen.getByLabelText("Turns per member per day"), "unlimited");
    await user.click(screen.getByRole("button", { name: "Save access" }));
    await waitFor(() => expect(setAccess).toHaveBeenCalledWith("ag-1", "everyone", 0, null, false));
    setAccess.mockClear();
    await user.selectOptions(screen.getByLabelText("Turns per member per day"), "default");
    await user.selectOptions(screen.getByLabelText("Who can use this agent"), "owner");
    await user.click(screen.getByRole("button", { name: "Save access" }));
    await waitFor(() => expect(setAccess).toHaveBeenCalledWith("ag-1", "owner", null, null, false));
  });

  it("folds the editor under a one-line summary", () => {
    renderPanel(base, false);
    const toggle = screen.getByRole("button", { name: /^Access/ });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).toHaveTextContent(accessSummary(base));
    expect(screen.queryByLabelText("Who can use this agent")).toBeNull();
    fireEvent.click(toggle);
    expect(screen.getByLabelText("Who can use this agent")).toBeInTheDocument();
  });

  it("a shared agent's restricted option is Admins only", () => {
    renderPanel({ ...base, ownerActorId: "", access: "everyone", effectiveMemberTurnsPerDay: null });
    expect(screen.getByRole("option", { name: "Admins only" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Default (no limit)" })).toBeInTheDocument();
  });

  it("someone who can't manage it sees a read-only badge and no controls", () => {
    renderPanel({ ...base, canManageAccess: false });
    expect(screen.getByText("Only its owner")).toBeInTheDocument();
    expect(screen.queryByLabelText("Who can use this agent")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save access" })).toBeNull();
    expect(accessLabel({ ...base, access: "everyone" })).toBe("Everyone");
    expect(accessLabel({ ...base, ownerActorId: "" })).toBe("Admins only");
  });

  it("the usage section shows turns today and this month, and cost where known", async () => {
    const user = userEvent.setup();
    renderPanel(base);
    await user.click(screen.getByRole("button", { name: "Usage on this device" }));
    const table = await screen.findByRole("table", { name: "Agent usage on this device" });
    const mate = screen.getByText("Mate").closest("tr")!;
    expect(mate.textContent).toContain("2");
    expect(mate.textContent).toContain("7");
    expect(mate.textContent).toContain("$0.75");
    expect(screen.getByText("Cli").closest("tr")!.textContent).toContain("not reported");
    expect(table).toBeInTheDocument();
    expect(screen.getByText(/counted in turns only/)).toBeInTheDocument();
  });

  it("parses dollar caps into cents", () => {
    expect(parseUsdCents("")).toBeNull();
    expect(parseUsdCents("$5")).toBe(500);
    expect(parseUsdCents("2.5")).toBe(250);
    expect(parseUsdCents("abc")).toBeUndefined();
    expect(parseUsdCents("1.234")).toBeUndefined();
  });
});
