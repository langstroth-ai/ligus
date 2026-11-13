import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DearthStateDto } from "@/bindings/DearthStateDto";

const state: { d: DearthStateDto } = { d: null as unknown as DearthStateDto };
const setDearthMode = vi.fn(async () => {});
const approveJoin = vi.fn(async () => {});
vi.mock("@/lib/ipc", () => ({
  getHygieneReport: vi.fn(async () => ({ ranAt: "", repoBound: false, turnWorktreesRemoved: 0, chatCheckoutsRemoved: 0, staleProposals: [] })),
  runHygieneNow: vi.fn(async () => ({ ranAt: "2026-11-13T00:00:00Z", repoBound: true, turnWorktreesRemoved: 2, chatCheckoutsRemoved: 0, staleProposals: [] })),
  getDearthState: vi.fn(async () => state.d),
  setDearthMode: (...a: unknown[]) => setDearthMode(...(a as [])),
  approveJoin: (...a: unknown[]) => approveJoin(...(a as [])),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { DearthBanner, dearthSummary } from "./DearthBanner";
import { GuardsSection, hygieneSummary } from "./GuardsSection";

const base: DearthStateDto = {
  enabled: false,
  reason: "",
  broodPaths: [".github/**"],
  allowedMcpServers: [],
  setBy: "",
  setAt: "",
  canManage: true,
  defaultBroodPaths: [".github/**"],
  pending: [],
  entrances: [{ name: "linear", kind: "mcp", state: "open" }],
};

const wrap = (ui: React.ReactNode) =>
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>{ui}</QueryClientProvider>);

beforeEach(() => {
  setDearthMode.mockClear();
  approveJoin.mockClear();
});

describe("dearth banner", () => {
  it("is hidden when off and visible when on", async () => {
    state.d = base;
    const { unmount } = wrap(<DearthBanner onManage={() => {}} />);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByTestId("dearth-banner")).toBeNull();
    unmount();
    state.d = { ...base, enabled: true, reason: "leaked invite", pending: [{ subject: "x", kind: "device", name: "Device x", addedBy: "Ada", canApprove: true }] };
    const onManage = vi.fn();
    wrap(<DearthBanner onManage={onManage} />);
    await waitFor(() => expect(screen.getByTestId("dearth-banner")).toBeInTheDocument());
    expect(screen.getByText("(leaked invite)")).toBeInTheDocument();
    expect(screen.getByText("1 waiting for approval")).toBeInTheDocument();
    await userEvent.click(screen.getByText("Manage"));
    expect(onManage).toHaveBeenCalled();
  });

  it("summarizes the restrictions", () => {
    expect(dearthSummary({ ...base, allowedMcpServers: [] })).toContain("no network tools");
    expect(dearthSummary({ ...base, allowedMcpServers: ["a", "b"] })).toContain("2 allow-listed MCP servers");
  });
});

describe("guards settings", () => {
  it("switches dearth mode on with the edited lists", async () => {
    state.d = base;
    wrap(<GuardsSection />);
    await waitFor(() => expect(screen.getByText("Switch on")).toBeInTheDocument());
    await userEvent.type(screen.getByPlaceholderText("e.g. invite link leaked"), "audit");
    await userEvent.click(screen.getByText("Switch on"));
    expect(setDearthMode).toHaveBeenCalledWith(true, "audit", [".github/**"], []);
  });

  it("hides the switch from non-admins and only offers approvals that count", async () => {
    state.d = {
      ...base,
      enabled: true,
      canManage: false,
      pending: [
        { subject: "m1", kind: "member", name: "Bob", addedBy: "Ada", canApprove: false },
      ],
    };
    wrap(<GuardsSection />);
    await waitFor(() => expect(screen.getByText("Bob")).toBeInTheDocument());
    expect(screen.queryByText("Switch off")).toBeNull();
    expect(screen.queryByText("Approve")).toBeNull();
    expect(screen.getByText("needs another Admin")).toBeInTheDocument();
  });
});

describe("hygiene", () => {
  it("summarizes a sweep", () => {
    const r = { ranAt: "x", repoBound: true, turnWorktreesRemoved: 1, chatCheckoutsRemoved: 0, staleProposals: [] };
    expect(hygieneSummary(r)).toBe("1 leftover worktree removed, 0 stale proposals set aside.");
    expect(hygieneSummary({ ...r, repoBound: false })).toContain("No git repository");
    expect(hygieneSummary({ ...r, ranAt: "" })).toContain("No sweep");
  });

  it("sweeps on demand", async () => {
    state.d = base;
    wrap(<GuardsSection />);
    await userEvent.click(await screen.findByText("Sweep now"));
    await waitFor(() => expect(screen.getByTestId("hygiene-summary").textContent).toContain("2 leftover worktrees removed"));
  });
});
