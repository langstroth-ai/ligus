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
}));
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
});
