import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { OrgSignInSection } from "./OrgSignInSection";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  ligusAuthStatus: vi.fn(async () => null),
  ligusAuthStart: vi.fn(async () => ({
    userCode: "BCDF-GHJK",
    verificationUri: "https://auth.example.com/t/acme/device",
    verificationUriComplete: "https://auth.example.com/t/acme/device?user_code=BCDF-GHJK",
    interval: 0,
    expiresIn: 600,
  })),
  ligusAuthPoll: vi.fn(async () => "success"),
  ligusAuthSignOut: vi.fn(async () => {}),
  openExternal: vi.fn(async () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function renderIt() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <OrgSignInSection />
    </QueryClientProvider>,
  );
}

describe("OrgSignInSection", () => {
  beforeEach(() => vi.clearAllMocks());

  it("runs the device-code sign-in and says it is not membership", async () => {
    renderIt();
    expect(await screen.findByText(/an Admin still has to add you as a/)).toBeTruthy();
    await userEvent.type(screen.getByPlaceholderText(/auth.example.com/), "https://auth.example.com/t/acme");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(ipc.ligusAuthStart).toHaveBeenCalledWith("https://auth.example.com/t/acme");
    expect(ipc.openExternal).toHaveBeenCalledWith("https://auth.example.com/t/acme/device?user_code=BCDF-GHJK");
    expect(await screen.findByText("BCDF-GHJK")).toBeTruthy();
    await waitFor(() => expect(ipc.ligusAuthPoll).toHaveBeenCalled());
  });

  it("shows the signed-in tenant and signs out", async () => {
    vi.mocked(ipc.ligusAuthStatus).mockResolvedValue({
      authUrl: "https://auth.example.com/t/acme",
      tenant: "acme",
      sub: "acme:u1",
      workspaces: ["w1", "w2"],
      accessExpiresAt: 0,
      active: true,
    });
    renderIt();
    expect(await screen.findByText("acme")).toBeTruthy();
    expect(screen.getByText(/Relay access to 2 workspaces/)).toBeTruthy();
    await userEvent.click(screen.getByText("Sign out"));
    expect(ipc.ligusAuthSignOut).toHaveBeenCalled();
  });
});
