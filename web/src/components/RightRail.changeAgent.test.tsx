import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import * as ipc from "@/lib/ipc";

const agent = {
  id: "a1",
  name: "coder",
  runtimeId: "ollama-box",
  role: "",
  ownerActorId: "me",
  hostId: "",
  avatarUrl: null,
  avatarColorHex: null,
  access: "owner",
  memberTurnsPerDay: null,
  effectiveMemberTurnsPerDay: null,
  usdCentsPerDay: null,
  notifyOnUse: false,
  available: true,
  unavailableReason: null,
  canManageAccess: true,
};

vi.mock("@/lib/ipc", async (orig) => ({
  ...(await orig<typeof import("@/lib/ipc")>()),
  listRuntimes: vi.fn(async () => []),
  listAgents: vi.fn(async () => [agent]),
  listMcpServers: vi.fn(async () => []),
  listWorkspaceHosts: vi.fn(async () => []),
  workspaceRules4Status: vi.fn(async () => ({
    rules: 4,
    isOwner: true,
    changes: [],
    readiness: [],
    oldDevices: [],
    unknownDevices: [],
    endpointRuntimes: [{ runtimeId: "ollama-box", name: "Ollama on the box", agents: [{ id: "a1", name: "coder" }], movableHere: true }],
    pins: [],
  })),
  runtimeEndpointMove: vi.fn(async () => 1),
}));

import { ToolsPane } from "./RightRail";

function renderPane() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <ToolsPane sessionId="00000000-0000-4000-8000-000000000001" activeRuntimeId="" />
    </QueryClientProvider>,
  );
}

describe("an agent's ⋯ menu and its model server notice", () => {
  beforeEach(() => vi.clearAllMocks());

  it("offers Change model… and Runs on… for an agent you manage", async () => {
    renderPane();
    await userEvent.click(await screen.findByRole("button", { name: "More actions for @coder" }));
    expect(await screen.findByText("Change model…")).toBeInTheDocument();
    expect(screen.getByText("Runs on…")).toBeInTheDocument();
  });

  it("says once that the address is visible to every member, with the move as its one action", async () => {
    renderPane();
    const notice = await screen.findByTestId("endpoint-notice-coder");
    expect(notice).toHaveTextContent("Its model server's address is visible to every member.");
    await userEvent.click(screen.getByRole("button", { name: "Move it to this computer" }));
    await waitFor(() => expect(ipc.runtimeEndpointMove).toHaveBeenCalledWith("ollama-box"));
  });
});
