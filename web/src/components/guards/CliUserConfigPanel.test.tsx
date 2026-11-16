import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const state = { on: false };
const setOn = vi.fn(async (v: boolean) => {
  state.on = v;
});
vi.mock("@/lib/ipc", () => ({
  getWorkspaceCliUserConfig: vi.fn(async () => state.on),
  setWorkspaceCliUserConfig: (v: boolean) => setOn(v),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { CliUserConfigPanel } from "./CliUserConfigPanel";

describe("CliUserConfigPanel", () => {
  it("is off by default and turns the workspace opt-in on", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <CliUserConfigPanel />
      </QueryClientProvider>,
    );
    const sw = await screen.findByRole("switch", { name: /Use my Claude Code settings/ });
    expect(sw.getAttribute("aria-checked")).toBe("false");
    await userEvent.click(sw);
    await waitFor(() => expect(setOn).toHaveBeenCalledWith(true));
    await waitFor(() => expect(screen.getByRole("switch", { name: /Use my Claude Code settings/ }).getAttribute("aria-checked")).toBe("true"));
  });
});
