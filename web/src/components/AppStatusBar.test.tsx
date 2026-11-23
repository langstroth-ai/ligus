import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AppStatusBar, workingCount } from "./AppStatusBar";
import { blockingBanner } from "@/lib/banners";
import { liveTurns } from "@/state/liveTurns";
import { _resetToasts, toast } from "@/components/Toast";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  syncStatus: vi.fn(async () => ({ relayConfigured: true, relayUrl: "wss://r", room: "apiary", encrypted: true, connectionState: "live", lastError: null })),
  listQueuedWork: vi.fn(async () => [{ sessionId: "c2", chatTitle: "Relay", agentId: "a", agentName: "builder", hostId: "h", hostLabel: "box", hostStatus: "offline" }]),
  growthSuggestions: vi.fn(async () => []),
  checkForAppUpdate: vi.fn(async () => null),
  onUpdateProgress: vi.fn(async () => () => {}),
  installUpdate: vi.fn(async () => {}),
  openExternal: vi.fn(async () => {}),
}));

function mount(props: Partial<Parameters<typeof AppStatusBar>[0]> = {}) {
  const p = { onOpenSettings: vi.fn(), onOpenReview: vi.fn(), onShowErrors: vi.fn(), ...props };
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <AppStatusBar {...p} />
    </QueryClientProvider>,
  );
  return p;
}

describe("blockingBanner", () => {
  it("allows one banner, a lockdown before dearth mode", () => {
    expect(blockingBanner({ lockdown: false, dearth: false })).toBeNull();
    expect(blockingBanner({ lockdown: false, dearth: true })).toBe("dearth");
    expect(blockingBanner({ lockdown: true, dearth: true })).toBe("lockdown");
  });
});

describe("AppStatusBar", () => {
  beforeEach(() => {
    liveTurns._reset();
    _resetToasts();
  });

  it("counts chats with an agent working", () => {
    expect(workingCount(new Map([["a", { live: true }], ["b", { live: false }], ["c", { live: true }]]))).toBe(2);
  });

  it("shows sync, encryption, work in progress and queued work, quietly", async () => {
    liveTurns.onStream({ sessionId: "c1", messageId: "m1", phase: "delta", text: "" });
    const p = mount();
    const bar = screen.getByRole("contentinfo", { name: "Status" });
    expect(await screen.findByText("Live · room apiary")).toBeInTheDocument();
    expect(screen.getByText("1 agent working")).toBeInTheDocument();
    expect(bar.querySelector('[role="alert"]')).toBeNull();
    await userEvent.click(await screen.findByText("1 queued"));
    expect(p.onOpenReview).toHaveBeenCalled();
    await userEvent.click(screen.getByText("Live · room apiary"));
    expect(p.onOpenSettings).toHaveBeenCalledWith("team-sync");
  });

  it("lists unseen errors and the needs-you count only when asked to", async () => {
    const p = mount({ inboxCount: 3, onOpenInbox: vi.fn() });
    expect(screen.queryByText(/error/)).toBeNull();
    act(() => {
      toast.error("Couldn't send");
    });
    await userEvent.click(await screen.findByText("1 error"));
    expect(p.onShowErrors).toHaveBeenCalled();
    await userEvent.click(screen.getByText("3 need you"));
    expect(p.onOpenInbox).toHaveBeenCalled();
  });

  it("says dearth mode is on when a lockdown holds the banner", async () => {
    const p = mount({ dearthAside: true });
    await userEvent.click(screen.getByText("Dearth mode"));
    expect(p.onOpenSettings).toHaveBeenCalledWith("guards");
  });

  it("offers an update from the bar, with Later and Skip in its dialog", async () => {
    vi.mocked(ipc.checkForAppUpdate).mockResolvedValueOnce({ tag: "v0.2.0", name: "Ligus 0.2.0", url: "u", notes: "Calmer." } as never);
    mount();
    await userEvent.click(await screen.findByText("Update available"));
    expect(screen.getByRole("dialog")).toHaveTextContent("Ligus 0.2.0");
    await userEvent.click(screen.getByRole("button", { name: "Skip this version" }));
    await waitFor(() => expect(screen.queryByText("Update available")).toBeNull());
    expect(window.localStorage.getItem("ligus.updateDismissed")).toBe("v0.2.0");
  });

  it("stays mounted but hidden in focus mode", () => {
    mount({ hidden: true });
    expect(screen.queryByRole("contentinfo")).toBeNull();
    expect(document.querySelector("footer[hidden]")).not.toBeNull();
  });
});
