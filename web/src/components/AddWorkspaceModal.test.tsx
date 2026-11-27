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
  inviteLinkHint: vi.fn(async () => false),
  linkRequestStatus: vi.fn(async () => ({ phase: "idle" })),
  onLinkFlowChanged: vi.fn(async () => () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function renderIt(onClose = vi.fn()) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <AddWorkspaceModal open onClose={onClose} />
    </QueryClientProvider>,
  );
}

const getConn = ipc.getConnectionSettings as unknown as ReturnType<typeof vi.fn>;

describe("AddWorkspaceModal relay choice", () => {
  beforeEach(() => vi.clearAllMocks());

  it("asks before closing half-way through Secure, and Keep going returns to it", async () => {
    getConn.mockResolvedValue(conn("https://relay.acme.example"));
    const onClose = vi.fn();
    renderIt(onClose);
    await userEvent.type(await screen.findByPlaceholderText(/Workspace name/), "Acme");
    await userEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    await screen.findByTestId("secure-workspace");
    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByText("Finish later?")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Keep going" }));
    expect(screen.getByTestId("secure-workspace")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    await userEvent.click(screen.getByRole("button", { name: "Finish later" }));
    expect(onClose).toHaveBeenCalledOnce();
  });

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

  it("offers a Personal workspace for outside prompts, made only on request", async () => {
    getConn.mockResolvedValue(conn("https://relay.acme.example"));
    renderIt();
    const row = await screen.findByTestId("personal-workspace");
    expect(row.textContent).toMatch(/visible to every member/);
    expect(ipc.createWorkspace).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Make a Personal workspace" }));
    await waitFor(() => expect(ipc.createWorkspace).toHaveBeenCalledWith("Personal"));
  });
});

describe("AddWorkspaceModal invite guard", () => {
  beforeEach(() => vi.clearAllMocks());
  const hint = ipc.inviteLinkHint as unknown as ReturnType<typeof vi.fn>;
  const join = ipc.joinWorkspace as unknown as ReturnType<typeof vi.fn>;

  it("hints to link this device for an invite that looks like your own, and still joins", async () => {
    getConn.mockResolvedValue(conn("https://relay.acme.example"));
    hint.mockResolvedValue(true);
    join.mockResolvedValue({ id: "w2", name: "Acme", kind: "room", active: true, iconUrl: null });
    const onClose = vi.fn();
    renderIt(onClose);
    await userEvent.click(screen.getByRole("button", { name: "Join" }));
    await userEvent.type(screen.getByPlaceholderText("hivews1:…"), "hivews1:abc");
    expect(await screen.findByTestId("invite-link-hint")).toHaveTextContent(
      "Looks like you already use Ligus. Link this device instead",
    );
    expect(hint).toHaveBeenLastCalledWith("hivews1:abc");
    // A hint, not a refusal: Join still works.
    await userEvent.click(screen.getByRole("button", { name: "Join workspace" }));
    await waitFor(() => expect(join).toHaveBeenCalledWith("hivews1:abc"));
  });

  it("opens the link screen from the hint, and Back returns", async () => {
    getConn.mockResolvedValue(conn("https://relay.acme.example"));
    hint.mockResolvedValue(true);
    renderIt();
    await userEvent.click(screen.getByRole("button", { name: "Join" }));
    await userEvent.type(screen.getByPlaceholderText("e.g. K7P2QX"), "K7P2QX");
    await userEvent.click(await screen.findByRole("button", { name: "Link this device instead" }));
    await screen.findByTestId("link-this-device");
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByPlaceholderText("e.g. K7P2QX")).toBeInTheDocument();
    expect(join).not.toHaveBeenCalled();
  });

  it("shows nothing for someone else's invite", async () => {
    getConn.mockResolvedValue(conn("https://relay.acme.example"));
    hint.mockResolvedValue(false);
    renderIt();
    await userEvent.click(screen.getByRole("button", { name: "Join" }));
    await userEvent.type(screen.getByPlaceholderText("hivews1:…"), "hivews1:abc");
    await waitFor(() => expect(hint).toHaveBeenCalled());
    expect(screen.queryByTestId("invite-link-hint")).toBeNull();
  });
});
