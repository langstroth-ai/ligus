import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { OldAgentCopyNotice } from "./OldAgentCopyNotice";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({ chatOldAgentCopies: vi.fn() }));

function renderIt() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <OldAgentCopyNotice sessionId="chat-1" />
    </QueryClientProvider>,
  );
}

describe("OldAgentCopyNotice", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
  });

  it("names the agent and hides itself on this device without writing anything", async () => {
    vi.mocked(ipc.chatOldAgentCopies).mockResolvedValue([{ id: "a1", name: "scout" }]);
    renderIt();
    expect(await screen.findByText("This chat has an old copy of @scout.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Use the workspace’s @scout" }));
    await waitFor(() => expect(screen.queryByTestId("old-agent-copy")).toBeNull());
    // Hidden again on the next render (this device only).
    renderIt();
    await waitFor(() => expect(ipc.chatOldAgentCopies).toHaveBeenCalledTimes(2));
    expect(screen.queryByTestId("old-agent-copy")).toBeNull();
  });

  it("shows nothing when the chat has no old copy (or the person isn't an Admin)", async () => {
    vi.mocked(ipc.chatOldAgentCopies).mockResolvedValue([]);
    renderIt();
    await waitFor(() => expect(ipc.chatOldAgentCopies).toHaveBeenCalled());
    expect(screen.queryByTestId("old-agent-copy")).toBeNull();
  });
});
