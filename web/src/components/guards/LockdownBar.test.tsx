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

import { LockdownBar, revealLockdownCause, signalLabel } from "./LockdownBar";

const wrap = (ui: React.ReactNode) =>
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>{ui}</QueryClientProvider>);

describe("LockdownBar", () => {
  it("explains the lockdown and lets a human member clear the signals shown", async () => {
    state.d = {
      active: true,
      signals: [
        { signal: "secretInDiff", detail: "aws_access_key", raisedAt: "", raiseEventId: "r1" },
        { signal: "manual", detail: "", raisedAt: "", raiseEventId: "r2" },
      ],
      canClear: true,
    };
    wrap(<LockdownBar sessionId="s1" />);
    await waitFor(() => expect(screen.getByTestId("lockdown-bar")).toBeInTheDocument());
    expect(screen.getByText(/credential in an agent's changes \(aws_access_key\)/)).toBeInTheDocument();
    await userEvent.click(screen.getByText("Clear lockdown"));
    // Exactly the raises it showed: one raised since stays in force.
    expect(clear).toHaveBeenCalledWith("s1", ["r1", "r2"]);
  });

  it("offers no clear button to someone who can't clear it", async () => {
    state.d = { active: true, signals: [{ signal: "injectionDetected", detail: "pipe_to_shell", raisedAt: "" }], canClear: false };
    wrap(<LockdownBar sessionId="s2" />);
    await waitFor(() => expect(screen.getByTestId("lockdown-bar")).toBeInTheDocument());
    expect(screen.queryByText("Clear lockdown")).toBeNull();
  });

  it("reveals the cause inside the reply it names, not a same-named call elsewhere", () => {
    document.body.innerHTML = `
      <div class="tt" data-message-id="a"><details class="tcall"><summary><span class="tcall-n">fetch_url</span></summary></details></div>
      <div class="tt" data-message-id="b"><details class="tcall"><summary><span class="tcall-n">fetch_url</span></summary></details></div>
      <div class="tt" data-message-id="c"><p>no tool calls</p></div>`;
    for (const el of Array.from(document.querySelectorAll<HTMLElement>(".tt, details"))) el.scrollIntoView = () => {};
    window.matchMedia = ((q: string) => ({ matches: false, media: q })) as unknown as typeof window.matchMedia;
    const [inA, inB] = Array.from(document.querySelectorAll<HTMLDetailsElement>("details.tcall"));
    expect(revealLockdownCause("fetch_url result", "a")).toBe(true);
    expect(inA.open && inA.classList.contains("hive-flash")).toBe(true);
    expect(inB.open || inB.classList.contains("hive-flash")).toBe(false);
    // A reply with no tool call (a credential in its changes): the reply itself.
    expect(revealLockdownCause("aws_access_key", "c")).toBe(true);
    expect(document.querySelector('[data-message-id="c"]')!.classList.contains("hive-flash")).toBe(true);
    // A reply that isn't loaded: nothing else is picked by mistake.
    expect(revealLockdownCause("fetch_url result", "gone")).toBe(false);
    // No id (an older lockdown): the newest same-named call.
    inB.open = false;
    expect(revealLockdownCause("fetch_url result")).toBe(true);
    expect(inB.open).toBe(true);
    document.body.innerHTML = "";
  });

  it("labels each signal", () => {
    expect(signalLabel({ signal: "ciConfigChanged", detail: "1 file", raisedAt: "" })).toContain("CI");
    expect(signalLabel({ signal: "manual", detail: "", raisedAt: "" })).toContain("member");
  });
});
