import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import * as ipc from "@/lib/ipc";

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

vi.mock("@/lib/ipc", async (orig) => ({
  ...(await orig<typeof import("@/lib/ipc")>()),
  listRuntimes: vi.fn(async () => [runtime("cc", "Claude Code"), runtime("gw", "Gateway model", "litellm")]),
  listAgentTemplates: vi.fn(async () => []),
  listAgents: vi.fn(async () => []),
  listMcpServers: vi.fn(async () => []),
  listWorkspaceHosts: vi.fn(async () => [
    { id: "me", kind: "device", label: "Desk", online: true, lastSeen: "", modelGateways: [], ownerName: "", mine: true },
    {
      id: "box",
      kind: "worker",
      label: "serve-1",
      online: true,
      lastSeen: "",
      modelGateways: [{ name: "litellm", api: "openai" }],
      ownerName: "",
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

  it("binds the host in the same write, defaulting a gateway runtime to the host that offers it", async () => {
    renderPane();
    await userEvent.click(await screen.findByRole("button", { name: "Add an agent" }));
    await userEvent.type(screen.getByPlaceholderText("Agent name"), "gw-bot");
    const runsOn = screen.getByLabelText("Agent host") as HTMLSelectElement;
    expect(runsOn.value).toBe("");
    await userEvent.selectOptions(screen.getByLabelText("Agent runtime"), "gw");
    await waitFor(() => expect(runsOn.value).toBe("box"));
    // This device isn't offered, and the serve box is listed.
    expect([...runsOn.options].map((o) => o.textContent)).toEqual(["This device", "serve-1"]);
    await userEvent.click(screen.getByRole("button", { name: "Add agent" }));
    await waitFor(() =>
      expect(ipc.addAgent).toHaveBeenCalledWith("00000000-0000-4000-8000-000000000001", "gw-bot", "gw", "", "box"),
    );
    expect(ipc.setAgentHost).not.toHaveBeenCalled();
  });

  it("says when the picked host can't reach the runtime's gateway", async () => {
    renderPane();
    await userEvent.click(await screen.findByRole("button", { name: "Add an agent" }));
    await userEvent.selectOptions(screen.getByLabelText("Agent runtime"), "gw");
    await waitFor(() => expect((screen.getByLabelText("Agent host") as HTMLSelectElement).value).toBe("box"));
    expect(screen.queryByTestId("agent-host-gateway-note")).toBeNull();
    await userEvent.selectOptions(screen.getByLabelText("Agent host"), "");
    expect(screen.getByTestId("agent-host-gateway-note")).toHaveTextContent("litellm");
  });
});
