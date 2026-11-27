import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import * as ipc from "@/lib/ipc";
import { DialogHost } from "@/components/Dialog";
import { WorkspaceRail } from "./WorkspaceRail";

const LINE = "Others see you as asked to leave until an Admin removes you, and this device stops syncing the workspace.";
const REMINDER = "Leaving isn't revoking: your other devices and anything they already copied keep their keys. Revoke a lost device in My devices.";

vi.mock("@/lib/ipc", () => ({
  listWorkspaces: vi.fn(async () => [
    { id: "local", name: "Personal", kind: "local", active: true, iconUrl: null },
    { id: "w1", name: "Acme", kind: "room", active: false, iconUrl: null },
  ]),
  listAllMentionStates: vi.fn(async () => []),
  syncStatus: vi.fn(async () => ({ relayConfigured: true, room: "acme" })),
  setActiveWorkspace: vi.fn(async () => {}),
  setWorkspaceIcon: vi.fn(async () => {}),
  workspaceInvite: vi.fn(async () => ""),
  workspaceShareCode: vi.fn(async () => ({ code: "", expiresIn: 60 })),
  removeWorkspace: vi.fn(async () => {}),
  leaveWorkspace: vi.fn(async () => {}),
  leaveWorkspaceCheck: vi.fn(),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

const wrapper = ({ children }: { children: ReactNode }) => (
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    {children}
    <DialogHost />
  </QueryClientProvider>
);

async function openLeave() {
  render(<WorkspaceRail onJoinRoom={() => {}} />, { wrapper });
  const acme = await screen.findByRole("button", { name: "Acme" });
  await userEvent.pointer({ keys: "[MouseRight]", target: acme });
  await userEvent.click(await screen.findByRole("menuitem", { name: "Leave workspace" }));
}

describe("Leave workspace", () => {
  beforeEach(() => vi.clearAllMocks());

  it("says what happens in one line, that it isn't revoking, and leaves with the passkey", async () => {
    vi.mocked(ipc.leaveWorkspaceCheck).mockResolvedValue({ state: "passkey", refusal: null, line: LINE, reminder: REMINDER });
    await openLeave();
    expect(await screen.findByText(`${LINE} ${REMINDER}`)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Leave with passkey" }));
    await waitFor(() => expect(ipc.leaveWorkspace).toHaveBeenCalledWith("w1"));
    expect(ipc.removeWorkspace).not.toHaveBeenCalled();
  });

  it("where the workspace refuses, offers removing it from this device only", async () => {
    vi.mocked(ipc.leaveWorkspaceCheck).mockResolvedValue({
      state: "refused",
      refusal: "Couldn't leave: you're the last Owner. Make someone else an Owner first",
      line: LINE,
      reminder: REMINDER,
    });
    await openLeave();
    expect(await screen.findByText(/last Owner.*you stay a member/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Remove from this device" }));
    await waitFor(() => expect(ipc.removeWorkspace).toHaveBeenCalledWith("w1"));
    expect(ipc.leaveWorkspace).not.toHaveBeenCalled();
  });
});
