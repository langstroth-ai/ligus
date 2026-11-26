import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SecuritySection } from "./SecuritySection";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  deviceConfirm: vi.fn(),
  passkeyRegister: vi.fn(),
  passkeyRemove: vi.fn(),
  recoveryKeyCreate: vi.fn(),
  rotateWorkspaceKey: vi.fn(),
  securityStatus: vi.fn(),
  workspaceRulesSet: vi.fn(async () => {}),
}));
vi.mock("@/components/Dialog", () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() }, errMsg: (e: unknown) => String(e) }));

function renderIt() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <SecuritySection />
    </QueryClientProvider>,
  );
}

describe("SecuritySection", () => {
  beforeEach(() => vi.clearAllMocks());

  // Final review: while loading, and when the status failed to load, the
  // Passkeys tab said "No passkeys yet." on a security screen.
  it("never claims there are no passkeys when it couldn't read them", async () => {
    vi.mocked(ipc.securityStatus).mockRejectedValue(new Error("the service is busy"));
    renderIt();
    expect(await screen.findByRole("alert")).toHaveTextContent("the service is busy");
    expect(screen.queryByText("No passkeys yet.")).toBeNull();
  });

  it("offers an Owner the switch to rules 3 with its readiness list", async () => {
    vi.mocked(ipc.securityStatus).mockResolvedValue({
      passkeys: [],
      rules: 2,
      keyVersion: 0,
      secretBackend: "keychain",
      deviceConfirmed: true,
      isOwner: true,
      rules3Gaps: ["Ben's phone 3c1e0f2a would stop counting until it confirms"],
    });
    renderIt();
    expect((await screen.findByTestId("rules3-gaps")).textContent).toContain("Ben's phone");
    screen.getByRole("button", { name: "Switch to rules 3" }).click();
    await vi.waitFor(() => expect(ipc.workspaceRulesSet).toHaveBeenCalledWith(3));
  });

  it("shows nothing about rules 3 to a non-Owner or on rules 3", async () => {
    vi.mocked(ipc.securityStatus).mockResolvedValue({ passkeys: [], rules: 3, keyVersion: 0, secretBackend: "keychain", deviceConfirmed: true, isOwner: true, rules3Gaps: [] });
    renderIt();
    await screen.findByText(/Key version/);
    expect(screen.queryByText("Switch to rules 3")).toBeNull();
  });
});
