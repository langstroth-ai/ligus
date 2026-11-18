import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TeamSyncSection } from "./TeamSyncSection";
import * as ipc from "@/lib/ipc";
import { COMMUNITY_RELAY_URL } from "@/lib/communityRelay";

vi.mock("@/lib/ipc", () => ({
  syncStatus: vi.fn(async () => ({ connectionState: "idle", lastError: null })),
  probeRelay: vi.fn(async () => ({ status: "ok", detail: "" })),
  getConnectionSettings: vi.fn(async () => ({
    relayUrl: "",
    room: "default",
    hasWorkspaceKey: false,
    hasApiKey: false,
    hasRelayAccessToken: false,
    permissionMode: "default",
  })),
  updateConnectionSettings: vi.fn(async () => ({})),
  p2pMyCode: vi.fn(async () => "LIG-1"),
  p2pListContacts: vi.fn(async () => []),
  p2pAddContact: vi.fn(),
  p2pRemoveContact: vi.fn(),
  p2pShareCode: vi.fn(),
  redeemShortCode: vi.fn(),
  hasIssuerKey: vi.fn(async () => false),
  setRelayIssuerKey: vi.fn(),
  ligusAuthStatus: vi.fn(async () => null),
  ligusAuthStart: vi.fn(),
  ligusAuthPoll: vi.fn(),
  ligusAuthSignOut: vi.fn(),
  openExternal: vi.fn(async () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

describe("Team sync relay URL", () => {
  beforeEach(() => vi.clearAllMocks());

  it("has no team-member admin panel, and fills in the community relay on request", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <TeamSyncSection />
      </QueryClientProvider>,
    );
    const use = await screen.findByRole("button", { name: "Use the community relay" });
    expect(screen.queryByText("Team members")).toBeNull();
    await userEvent.click(use);
    await waitFor(() =>
      expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
        expect.objectContaining({ relayUrl: COMMUNITY_RELAY_URL }),
      ),
    );
    expect(await screen.findByText(/best-effort/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Use the community relay" })).toBeNull();
  });
});
