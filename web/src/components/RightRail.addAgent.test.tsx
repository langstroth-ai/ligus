import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import * as ipc from "@/lib/ipc";
import * as nav from "@/lib/nav";

const runtime = (id: string, name: string, gateway: string | null = null) => ({
  id,
  name,
  label: name,
  provider: gateway ? "custom" : "claude-code",
  location: "local",
  model: "",
  endpoint: "",
  supportsTools: true,
  supportsEmbeddings: false,
  isDefault: false,
  isManaged: false,
  gateway,
});

vi.mock("@/lib/nav", async (orig) => ({
  ...(await orig<typeof import("@/lib/nav")>()),
  openSetup: vi.fn(),
}));

vi.mock("@/lib/ipc", async (orig) => ({
  ...(await orig<typeof import("@/lib/ipc")>()),
  listRuntimes: vi.fn(async () => [runtime("cc", "Claude Code"), runtime("gw", "Gateway model", "litellm")]),
  listAgentTemplates: vi.fn(async () => []),
  listAgents: vi.fn(async () => []),
  listMcpServers: vi.fn(async () => []),
  listWorkspaceHosts: vi.fn(async () => [
    { id: "me", kind: "device", label: "Desk", online: true, lastSeen: "", modelGateways: [], ownerName: "", mine: true, offers: [], readiness: "", build: "" },
    {
      id: "box",
      kind: "worker",
      label: "serve-1",
      online: true,
      lastSeen: "",
      modelGateways: [{ name: "litellm", api: "openai" }],
      ownerName: "",
      offers: [],
      readiness: "",
      build: "",
      mine: false,
    },
  ]),
  listMyDevices: vi.fn(async () => [{ deviceId: "me", label: "Desk", fingerprint: "", thisDevice: true, revoked: false, lastSeen: null }]),
  addAgent: vi.fn(async () => {}),
  setAgentHost: vi.fn(async () => {}),
}));

import { ToolsPane } from "./RightRail";

function renderPane() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <ToolsPane sessionId="00000000-0000-4000-8000-000000000001" activeRuntimeId="cc" />
    </QueryClientProvider>,
  );
}

describe("Add an agent", () => {
  beforeEach(() => vi.clearAllMocks());

  it("opens the one Add an agent sheet, with no dialog of its own", async () => {
    renderPane();
    await userEvent.click(await screen.findByRole("button", { name: "Add an agent" }));
    expect(nav.openSetup).toHaveBeenCalledWith("agent");
    // No second form here: no runtime-first picker, no name field.
    expect(screen.queryByPlaceholderText("Agent name")).toBeNull();
    expect(screen.queryByLabelText("Agent runtime")).toBeNull();
    expect(ipc.addAgent).not.toHaveBeenCalled();
    await waitFor(() => expect(ipc.listAgents).toHaveBeenCalled());
  });
});
