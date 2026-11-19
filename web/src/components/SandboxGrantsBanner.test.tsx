import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { SandboxPolicyDto } from "@/lib/ipc";

const state: { d: Partial<SandboxPolicyDto> } = { d: {} };
const ack = vi.fn(async () => {});
const open = vi.fn();
vi.mock("@/lib/ipc", () => ({ getSandboxPolicy: vi.fn(async () => state.d), ackSandboxGrants: () => ack() }));
vi.mock("@/lib/nav", () => ({ openSettingsTab: (t: string) => open(t) }));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { SandboxGrantsBanner } from "./SandboxGrantsBanner";

function renderBanner() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <SandboxGrantsBanner />
    </QueryClientProvider>,
  );
}

describe("SandboxGrantsBanner", () => {
  beforeEach(() => {
    ack.mockClear();
    open.mockClear();
  });

  it("says the grants changed and whether this device applies them; Review opens Settings, Sandbox", async () => {
    state.d = { changed: true, appliesHere: false, grants: { workspace: { readWrite: ["~/scratch"] } } };
    renderBanner();
    expect(await screen.findByText(/sandbox grants changed/)).toBeTruthy();
    expect(screen.getByText(/This device doesn't apply them/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Review" }));
    await waitFor(() => expect(ack).toHaveBeenCalled());
    expect(open).toHaveBeenCalledWith("sandbox");
  });

  it("stays hidden when nothing changed", async () => {
    state.d = { changed: false, appliesHere: true, grants: {} };
    const { container } = renderBanner();
    await new Promise((r) => setTimeout(r, 20));
    expect(container.textContent).toBe("");
  });
});
