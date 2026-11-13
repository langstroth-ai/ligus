import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ChatLockdownDto } from "@/bindings/ChatLockdownDto";

const state: { d: ChatLockdownDto } = { d: { active: false, signals: [], canClear: false } };
const clear = vi.fn(async () => {});
vi.mock("@/lib/ipc", () => ({
  getChatLockdown: vi.fn(async () => state.d),
  clearChatLockdown: (...a: unknown[]) => clear(...(a as [])),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { LockdownBar, signalLabel } from "./LockdownBar";

const wrap = (ui: React.ReactNode) =>
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>{ui}</QueryClientProvider>);

describe("LockdownBar", () => {
  it("explains the lockdown and lets a human member clear it", async () => {
    state.d = { active: true, signals: [{ signal: "secretInDiff", detail: "aws_access_key", raisedAt: "" }], canClear: true };
    wrap(<LockdownBar sessionId="s1" />);
    await waitFor(() => expect(screen.getByTestId("lockdown-bar")).toBeInTheDocument());
    expect(screen.getByText(/credential in an agent's changes \(aws_access_key\)/)).toBeInTheDocument();
    await userEvent.click(screen.getByText("Clear lockdown"));
    expect(clear).toHaveBeenCalledWith("s1");
  });

  it("offers no clear button to someone who can't clear it", async () => {
    state.d = { active: true, signals: [{ signal: "injectionDetected", detail: "pipe_to_shell", raisedAt: "" }], canClear: false };
    wrap(<LockdownBar sessionId="s2" />);
    await waitFor(() => expect(screen.getByTestId("lockdown-bar")).toBeInTheDocument());
    expect(screen.queryByText("Clear lockdown")).toBeNull();
  });

  it("labels each signal", () => {
    expect(signalLabel({ signal: "ciConfigChanged", detail: "1 file", raisedAt: "" })).toContain("CI");
    expect(signalLabel({ signal: "manual", detail: "", raisedAt: "" })).toContain("member");
  });
});
