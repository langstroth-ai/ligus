import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

// The backend's view of THIS data dir (settings.json). The webview's
// localStorage outlives a deleted data dir, so App must gate on this alone.
const backend = { onboarded: false, tourSeen: false, displayName: "You" };
const calls: string[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    calls.push(cmd);
    switch (cmd) {
      case "get_app_settings":
        return {
          displayName: backend.displayName,
          gitEmail: "",
          deviceName: "This device",
          workspaceRoot: "",
          workspaceFolder: "",
          knownWorkspaces: [],
          model: "",
          gitBranch: null,
          gitDirtyCount: 0,
          avatarUrl: null,
          onboarded: backend.onboarded,
          tourSeen: backend.tourSeen,
        };
      case "mark_onboarded":
        backend.onboarded = true;
        return null;
      case "mark_tour_seen":
        backend.tourSeen = true;
        return null;
      case "list_workspaces":
      case "list_runtimes":
      case "list_chats":
      case "list_channels":
        return [];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string) => p,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/components/Onboarding", () => ({
  Onboarding: ({ onComplete }: { onComplete: () => Promise<void> | void }) => (
    <button onClick={() => void onComplete()}>finish onboarding</button>
  ),
}));
vi.mock("@/components/Tour", () => ({
  Tour: ({ onClose }: { onClose: () => void }) => <button onClick={onClose}>close tour</button>,
}));

import { App } from "./App";

function renderApp() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <App />
    </QueryClientProvider>,
  );
}

describe("App first-run gate", () => {
  beforeEach(() => {
    Object.assign(backend, { onboarded: false, tourSeen: false, displayName: "You" });
    calls.length = 0;
  });

  it("a fresh data dir shows onboarding even when localStorage says onboarded", async () => {
    window.localStorage.setItem("ligus.onboarded", "1");
    window.localStorage.setItem("ligus.tourSeen", "1");
    backend.displayName = "Ana"; // a leftover name doesn't skip it either
    renderApp();
    expect(await screen.findByText("finish onboarding")).toBeInTheDocument();
  });

  it("completing onboarding records it in the backend and clears the old flags", async () => {
    window.localStorage.setItem("ligus.onboarded", "1");
    renderApp();
    fireEvent.click(await screen.findByText("finish onboarding"));
    await waitFor(() => expect(screen.queryByText("finish onboarding")).not.toBeInTheDocument());
    expect(calls).toContain("mark_onboarded");
    expect(window.localStorage.getItem("ligus.onboarded")).toBeNull();
    // The tour auto-runs once, and closing it is recorded in the backend.
    fireEvent.click(await screen.findByText("close tour", {}, { timeout: 3000 }));
    await waitFor(() => expect(calls).toContain("mark_tour_seen"));
  });

  it("a completed data dir shows no onboarding and no tour", async () => {
    Object.assign(backend, { onboarded: true, tourSeen: true });
    renderApp();
    await waitFor(() => expect(calls).toContain("get_app_settings"));
    await new Promise((r) => setTimeout(r, 700));
    expect(screen.queryByText("finish onboarding")).not.toBeInTheDocument();
    expect(screen.queryByText("close tour")).not.toBeInTheDocument();
  });
});
