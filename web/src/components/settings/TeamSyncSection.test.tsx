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

describe("Team sync limited state", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows a full workspace as syncing with writes waiting, not as an error", async () => {
    vi.mocked(ipc.syncStatus).mockResolvedValue({
      relayConfigured: true,
      relayUrl: "https://relay.example",
      room: "acme",
      encrypted: true,
      connectionState: "limited",
      lastError: "workspace full: the relay is not taking new writes for this workspace (room_bytes)",
    });
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <TeamSyncSection />
      </QueryClientProvider>,
    );
    expect(await screen.findByText("Syncing, but writes are waiting")).toBeTruthy();
    expect(screen.getByText(/workspace full/)).toBeTruthy();
    expect(screen.queryByText("Sync error")).toBeNull();
  });
});

describe("Team sync held events", () => {
  beforeEach(() => vi.clearAllMocks());

  it("says how many events wait for a certificate or admission, and how many were given up", async () => {
    vi.mocked(ipc.syncStatus).mockResolvedValue({
      relayConfigured: true,
      relayUrl: "https://relay.example",
      room: "acme",
      encrypted: true,
      connectionState: "live",
      lastError: null,
      heldEvents: 3,
      heldSigners: 2,
      heldDropped: 1,
    });
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <TeamSyncSection />
      </QueryClientProvider>,
    );
    const line = await screen.findByTestId("held-events");
    expect(line.textContent).toMatch(/3 events wait for a device\s+certificate or an admission \(2 devices\)/);
    expect(line.textContent).toMatch(/don't hold up sync/);
    expect(line.textContent).toMatch(/1 given up/);
  });

  it("shows nothing when nothing waits", async () => {
    vi.mocked(ipc.syncStatus).mockResolvedValue({
      relayConfigured: true,
      relayUrl: "https://relay.example",
      room: "acme",
      encrypted: true,
      connectionState: "live",
      lastError: null,
      heldEvents: 0,
      heldSigners: 0,
      heldDropped: 0,
    });
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <TeamSyncSection />
      </QueryClientProvider>,
    );
    await screen.findByText(/Live/);
    expect(screen.queryByTestId("held-events")).toBeNull();
  });
});
