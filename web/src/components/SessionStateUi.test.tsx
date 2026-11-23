import { describe, it, expect, vi } from "vitest";
import { render, renderHook, screen, waitFor, within } from "@testing-library/react";
import * as ipc from "@/lib/ipc";
import { useChatAttention } from "@/state/useSessionStates";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ChatRow, SessionStateChip } from "./ui";
import { ShortcutsSheet } from "./ShortcutsSheet";
import { runsOnParts, shortBranch } from "./ChatRepoChip";
import { SHORTCUTS, keyLabel } from "@/lib/shortcuts";
import { SESSION_STATE_META } from "@/lib/sessionState";

vi.mock("@/lib/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  listMyDevices: vi.fn(async () => []),
  listDeviceRequests: vi.fn(async () => []),
  listDeviceRevocations: vi.fn(async () => []),
  listDevicePairings: vi.fn(async () => []),
  listChatAttention: vi.fn(async () => []),
}));
vi.mock("@/components/settings/AddPhonePanel", () => ({
  AddPhonePanel: () => <div data-testid="add-phone-panel" />,
}));

describe("SessionStateChip", () => {
  it("shows a glyph and a word for every non-idle state", () => {
    for (const [state, meta] of Object.entries(SESSION_STATE_META)) {
      const { container, unmount } = render(<SessionStateChip state={state as keyof typeof SESSION_STATE_META} />);
      const chip = container.querySelector(`[data-session-state="${state}"]`)!;
      expect(chip).not.toBeNull();
      expect(chip.querySelector("svg")).not.toBeNull();
      expect(chip.textContent).toBe(meta.word);
      expect(chip.getAttribute("title")).toBe(meta.describe);
      unmount();
    }
  });

  it("renders nothing for an idle chat", () => {
    const { container } = render(<SessionStateChip state="idle" />);
    expect(container.innerHTML).toBe("");
  });
});

describe("ChatRow state", () => {
  it("shows the time when idle and the state chip in its place otherwise", () => {
    const { rerender } = render(<ChatRow chatId="c" title="Relay backoff" when="5m" />);
    expect(screen.getByText("5m")).toBeInTheDocument();
    rerender(<ChatRow chatId="c" title="Relay backoff" when="5m" state="needsYou" />);
    expect(screen.queryByText("5m")).not.toBeInTheDocument();
    expect(screen.getByText("Needs you")).toBeInTheDocument();
    rerender(<ChatRow chatId="c" title="Relay backoff" when="5m" state="idle" />);
    expect(screen.getByText("5m")).toBeInTheDocument();
  });
});

describe("ShortcutsSheet", () => {
  it("lists every shortcut from the shared list, with its keys", () => {
    render(<ShortcutsSheet onClose={() => {}} mac />);
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("heading", { name: "Keyboard shortcuts" })).toBeInTheDocument();
    for (const s of SHORTCUTS) {
      expect(within(dialog).getAllByText(s.label).length).toBeGreaterThan(0);
      expect(within(dialog).getAllByText(keyLabel(s.keys, true)).length).toBeGreaterThan(0);
    }
    expect(within(dialog).getByText("Open Activity")).toBeInTheDocument();
    expect(within(dialog).getByText("⌘⌥0")).toBeInTheDocument();
  });

  it("spells keys out off a Mac", () => {
    render(<ShortcutsSheet onClose={() => {}} mac={false} />);
    expect(screen.getByText("Ctrl+K")).toBeInTheDocument();
  });

  it("closes on Escape", async () => {
    const onClose = vi.fn();
    render(<ShortcutsSheet onClose={onClose} mac />);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(onClose).toHaveBeenCalled();
  });
});

describe("runs-on line", () => {
  it("shortens a chat's own branch and keeps other branches", () => {
    expect(shortBranch("ligus/chat/7f3a1c2e-1111-4222-8333-444455556666")).toBe("chat/7f3a1c2e");
    expect(shortBranch("hive/chat/7f3a1c2e-1111-4222-8333-444455556666")).toBe("chat/7f3a1c2e");
    expect(shortBranch("main")).toBe("main");
    expect(shortBranch("feature/chat-tidy")).toBe("feature/chat-tidy");
  });

  it("reads host, runtime, branch, leaving out what isn't known", () => {
    expect(runsOnParts({ host: "MacBook", runtime: "anthropic / claude", branch: "main" })).toEqual([
      "Runs on MacBook",
      "anthropic / claude",
      "main",
    ]);
    expect(runsOnParts({ host: "", runtime: null, branch: null })).toEqual(["Runs on this device"]);
  });
});

describe("Add a phone from the palette", () => {
  it("opens Profile with the add-a-phone panel already open, once", async () => {
    const { requestAddPhone } = await import("@/lib/nav");
    const { DevicesSection } = await import("./settings/AccountSection");
    const seen: string[] = [];
    const onOpen = (e: Event) => seen.push((e as CustomEvent<string>).detail);
    window.addEventListener("ligus:open-settings", onOpen);
    requestAddPhone();
    window.removeEventListener("ligus:open-settings", onOpen);
    expect(seen).toEqual(["account"]);

    const wrap = (ui: React.ReactNode) => (
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>{ui}</QueryClientProvider>
    );
    const first = render(wrap(<DevicesSection />));
    expect(screen.getByTestId("add-phone-panel")).toBeInTheDocument();
    first.unmount();
    // The request is used up: opening Profile again starts closed.
    render(wrap(<DevicesSection />));
    expect(screen.queryByTestId("add-phone-panel")).not.toBeInTheDocument();
  });
});

describe("useChatAttention", () => {
  it("refetches when a chat's questions change, not on every identical poll", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: React.ReactNode }) => (
      <QueryClientProvider client={qc}>{children}</QueryClientProvider>
    );
    const fetches = vi.mocked(ipc.listChatAttention);
    fetches.mockClear();
    renderHook(() => useChatAttention("w1"), { wrapper });
    await waitFor(() => expect(fetches).toHaveBeenCalledTimes(1));
    const asks = [{ askId: "a", open: true }];
    await qc.fetchQuery({ queryKey: ["asks", "c1"], queryFn: async () => asks });
    await waitFor(() => expect(fetches).toHaveBeenCalledTimes(2), { timeout: 2000 });
    // The same answer again (a poll): structural sharing keeps the object, no refetch.
    await qc.fetchQuery({ queryKey: ["asks", "c1"], queryFn: async () => [{ askId: "a", open: true }], staleTime: 0 });
    await new Promise((r) => setTimeout(r, 400));
    expect(fetches).toHaveBeenCalledTimes(2);
    // Answered: a real change refetches.
    await qc.fetchQuery({ queryKey: ["asks", "c1"], queryFn: async () => [{ askId: "a", open: false }], staleTime: 0 });
    await waitFor(() => expect(fetches).toHaveBeenCalledTimes(3), { timeout: 2000 });
  });
});
