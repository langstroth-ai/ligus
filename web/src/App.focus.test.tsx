import { describe, it, expect, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const calls: { cmd: string; args: unknown }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: unknown) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "get_app_settings":
        return {
          displayName: "Ana",
          gitEmail: "",
          deviceName: "This device",
          workspaceRoot: "",
          workspaceFolder: "",
          knownWorkspaces: [],
          model: "",
          gitBranch: null,
          gitDirtyCount: 0,
          avatarUrl: null,
          onboarded: true,
          tourSeen: true,
        };
      case "list_workspaces":
        return [{ id: "w1", name: "Apiary", kind: "local", active: true }];
      case "list_chat_attention":
        return [{ sessionId: "c1", openAsks: 2, paused: false, proposals: [] }];
      case "list_pending_invites":
        return { contacts: "on", invites: [] };
      default:
        return cmd.startsWith("list_") ? [] : null;
    }
  }),
  convertFileSrc: (p: string) => p,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/components/Tour", () => ({ Tour: () => null }));

import { App } from "./App";

function press(key: string, code: string) {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key, code, metaKey: true, bubbles: true }));
  });
}

describe("focus mode and the needs-you badge", () => {
  it("⌘. hides everything but the chat, and brings it all back", async () => {
    render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <App />
      </QueryClientProvider>,
    );
    expect(await screen.findByPlaceholderText("Search chats")).toBeInTheDocument();
    expect(screen.getByRole("contentinfo", { name: "Status" })).toBeInTheDocument();
    // The dock and the sidebar's Inbox row count the two questions.
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_needs_you_count" && (c.args as { count: number }).count === 2)).toBe(true));

    press(".", "Period");
    expect(screen.queryByPlaceholderText("Search chats")).toBeNull();
    expect(screen.queryByRole("contentinfo", { name: "Status" })).toBeNull();
    // One quiet badge and a way out stay.
    expect(screen.getByTestId("focus-controls")).toHaveTextContent("2 need you");

    press(".", "Period");
    expect(screen.getByPlaceholderText("Search chats")).toBeInTheDocument();
    expect(screen.getByRole("contentinfo", { name: "Status" })).toBeInTheDocument();
    expect(screen.queryByTestId("focus-controls")).toBeNull();
  });
});
