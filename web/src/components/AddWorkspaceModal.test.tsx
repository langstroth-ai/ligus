import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AddWorkspaceModal } from "./AddWorkspaceModal";
import * as ipc from "@/lib/ipc";
import { COMMUNITY_RELAY_URL } from "@/lib/communityRelay";

const conn = (relayUrl: string) => ({
  relayUrl,
  room: "default",
  hasWorkspaceKey: false,
  hasApiKey: false,
  hasRelayAccessToken: false,
  permissionMode: "default",
});

vi.mock("@/lib/ipc", () => ({
  listWorkspaces: vi.fn(async () => []),
  createWorkspace: vi.fn(async (name: string) => ({ id: "w1", name, kind: "room", active: true, iconUrl: null })),
  joinWorkspace: vi.fn(),
  workspaceInvite: vi.fn(async () => "hivews1:abc"),
  removeWorkspace: vi.fn(),
  workspaceShareCode: vi.fn(),
  redeemShortCode: vi.fn(),
  passkeyRegister: vi.fn(),
  recoveryKeyCreate: vi.fn(),
  getConnectionSettings: vi.fn(),
  openExternal: vi.fn(async () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function renderIt() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <AddWorkspaceModal open onClose={vi.fn()} />
    </QueryClientProvider>,
  );
}

const getConn = ipc.getConnectionSettings as unknown as ReturnType<typeof vi.fn>;

describe("AddWorkspaceModal relay choice", () => {
  beforeEach(() => vi.clearAllMocks());

  it("with no relay set, preselects the community relay and creates on it", async () => {
    getConn.mockResolvedValue(conn(""));
    renderIt();
    await screen.findByTestId("new-workspace-relay");
    const community = screen.getByRole("radio", { name: /Community relay/ });
    expect(community.getAttribute("aria-checked")).toBe("true");
    await userEvent.type(screen.getByPlaceholderText(/Workspace name/), "Acme");
    await userEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    await waitFor(() => expect(ipc.createWorkspace).toHaveBeenCalledWith("Acme", COMMUNITY_RELAY_URL));
  });

  it("creates on your own relay, and needs its URL first", async () => {
    getConn.mockResolvedValue(conn(""));
    renderIt();
    await screen.findByTestId("new-workspace-relay");
    await userEvent.type(screen.getByPlaceholderText(/Workspace name/), "Acme");
    await userEvent.click(screen.getByRole("radio", { name: /Your own relay/ }));
    expect(screen.getByRole("button", { name: "Create workspace" })).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Relay URL"), "https://relay.acme.example");
    await userEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    await waitFor(() => expect(ipc.createWorkspace).toHaveBeenCalledWith("Acme", "https://relay.acme.example"));
  });

  it("can create with no relay", async () => {
    getConn.mockResolvedValue(conn(""));
    renderIt();
    await screen.findByTestId("new-workspace-relay");
    await userEvent.type(screen.getByPlaceholderText(/Workspace name/), "Solo");
    await userEvent.click(screen.getByRole("radio", { name: /No relay/ }));
    await userEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    await waitFor(() => expect(ipc.createWorkspace).toHaveBeenCalledWith("Solo", ""));
  });

  it("with a relay already set, uses it and shows no picker", async () => {
    getConn.mockResolvedValue(conn("https://relay.acme.example"));
    renderIt();
    await screen.findByText(/Syncs through https:\/\/relay\.acme\.example/);
    expect(screen.queryByTestId("new-workspace-relay")).toBeNull();
    await userEvent.type(screen.getByPlaceholderText(/Workspace name/), "Acme");
    await userEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    await waitFor(() => expect(ipc.createWorkspace).toHaveBeenCalledWith("Acme"));
  });
});
