import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ToolsSection } from "./ToolsSection";
import * as ipc from "@/lib/ipc";
import { promptDialog } from "@/components/Dialog";

vi.mock("@/lib/ipc", () => ({
  listMcpServers: vi.fn(async () => [
    {
      id: "docs",
      transport: "http",
      detail: "http://10.0.0.5:8090/mcp",
      enabled: true,
      isManaged: true,
      hasBearerToken: true,
      oauthConnected: false,
      secretForOtherUrl: false,
      shellTools: ["run_command"],
    },
    {
      id: "linear",
      transport: "http",
      detail: "https://mcp.linear.app/sse",
      enabled: false,
      isManaged: true,
      hasBearerToken: false,
      oauthConnected: true,
      secretForOtherUrl: false,
      shellTools: null,
    },

    {
      id: "fs",
      transport: "stdio",
      detail: "mcp-fs",
      enabled: false,
      isManaged: true,
      hasBearerToken: false,
      oauthConnected: false,
      secretForOtherUrl: false,
      shellTools: null,
    },
  ]),
  installMcpServer: vi.fn(async () => {}),
  addRemoteMcpServer: vi.fn(async () => {}),
  authorizeMcpServer: vi.fn(async () => {}),
  setMcpOauthClient: vi.fn(async () => {}),
  removeMcpServer: vi.fn(async () => {}),
  setMcpEnabled: vi.fn(async () => {}),
  setMcpBearerToken: vi.fn(async (_id: string, token: string) => token !== ""),
  inspectMcpServer: vi.fn(async () => []),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

vi.mock("@/components/Dialog", () => ({
  promptDialog: vi.fn(async () => "  new-token  "),
}));

/// Rows are one line; their token/connect/check/remove actions live in the
/// row menu (the ⋯ button named "Actions for <id>").
function openMenu(id: string) {
  fireEvent.click(screen.getByRole("button", { name: `Actions for ${id}` }));
}
function menuItem(name: string) {
  return screen.getByRole("menuitem", { name });
}

function renderSection() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <ToolsSection />
    </QueryClientProvider>,
  );
}

describe("ToolsSection bearer tokens", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows token state without ever rendering a token value", async () => {
    renderSection();
    expect(await screen.findByText("· token set")).toBeTruthy();
    expect(screen.getByText("· connected")).toBeTruthy();
    expect(screen.queryByText("· token saved for a different URL")).toBeNull();
    expect(screen.queryByRole("button", { name: "Clear token" })).toBeNull();
    // Token controls only on HTTP servers, in the row menu.
    openMenu("docs");
    expect(menuItem("Clear token")).toBeTruthy();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menuitem")).toBeNull());
    openMenu("fs");
    expect(screen.queryByRole("menuitem", { name: "Set token" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Clear token" })).toBeNull();
  });

  it("flags a token saved for a different URL under the same id", async () => {
    vi.mocked(ipc.listMcpServers).mockResolvedValueOnce([
      {
        id: "docs",
        transport: "http",
        detail: "https://elsewhere.example/mcp",
        enabled: true,
        isManaged: true,
        hasBearerToken: true,
        oauthConnected: false,
        secretForOtherUrl: true,
        shellTools: [],
      },
    ]);
    renderSection();
    expect(await screen.findByText("· token saved for a different URL")).toBeTruthy();
    // It can still be cleared or replaced.
    openMenu("docs");
    expect(menuItem("Clear token")).toBeTruthy();
  });

  it("adds an HTTP server with a password-type bearer field", async () => {
    const user = userEvent.setup();
    renderSection();
    await screen.findByText("docs");
    await user.click(screen.getByRole("button", { name: "Add a server" }));
    await user.click(screen.getByRole("button", { name: /Add HTTP server/ }));
    const token = screen.getByLabelText(/Bearer token \(optional\)/) as HTMLInputElement;
    expect(token.type).toBe("password");
    await user.type(screen.getByLabelText("Name"), "rdna");
    await user.type(screen.getByLabelText("URL"), "http://10.0.0.9:8090/mcp");
    await user.type(token, "s3cret");
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await waitFor(() =>
      expect(ipc.addRemoteMcpServer).toHaveBeenCalledWith("rdna", "http://10.0.0.9:8090/mcp", "s3cret"),
    );
  });

  it("omits the token when left blank (keeps any saved one)", async () => {
    const user = userEvent.setup();
    renderSection();
    await screen.findByText("docs");
    await user.click(screen.getByRole("button", { name: "Add a server" }));
    await user.click(screen.getByRole("button", { name: /Add HTTP server/ }));
    await user.type(screen.getByLabelText("Name"), "open");
    await user.type(screen.getByLabelText("URL"), "http://h/mcp");
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await waitFor(() =>
      expect(ipc.addRemoteMcpServer).toHaveBeenCalledWith("open", "http://h/mcp", undefined),
    );
  });

  it("sets and clears a token on an existing server", async () => {
    const user = userEvent.setup();
    renderSection();
    await screen.findByText("docs");
    openMenu("linear");
    await user.click(menuItem("Set token"));
    await waitFor(() => expect(ipc.setMcpBearerToken).toHaveBeenCalledWith("linear", "  new-token  "));
    expect(promptDialog).toHaveBeenCalledWith(
      expect.stringContaining("linear"),
      expect.objectContaining({ password: true }),
    );
    openMenu("docs");
    await user.click(menuItem("Clear token"));
    await waitFor(() => expect(ipc.setMcpBearerToken).toHaveBeenCalledWith("docs", ""));
  });

  it("flags a server that offers a shell, and checks an unchecked one's tools", async () => {
    const user = userEvent.setup();
    vi.mocked(ipc.listMcpServers).mockResolvedValueOnce([
      { id: "box", transport: "stdio", detail: "box-mcp", enabled: true, isManaged: true, hasBearerToken: false, oauthConnected: false, secretForOtherUrl: false, shellTools: ["bash", "exec"] },
      { id: "new", transport: "stdio", detail: "new-mcp", enabled: true, isManaged: true, hasBearerToken: false, oauthConnected: false, secretForOtherUrl: false, shellTools: null },
    ]);
    renderSection();
    expect(await screen.findByText("· offers a shell (bash, exec): Owner turns only")).toBeTruthy();
    expect(screen.getByText("· tools not checked yet")).toBeTruthy();
    openMenu("new");
    await user.click(menuItem("Check tools"));
    await waitFor(() => expect(ipc.inspectMcpServer).toHaveBeenCalledWith("new"));
  });
});
