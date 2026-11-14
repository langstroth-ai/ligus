import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, render, screen } from "@testing-library/react";

const handlers: Record<string, (e: { payload: unknown }) => void> = {};
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, cb: (e: { payload: unknown }) => void) => {
    handlers[name] = cb;
    return Promise.resolve(() => {});
  }),
}));
const invoke = vi.fn(() => Promise.resolve());
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...(a as [])) }));

import { PresenceSheet } from "./PresenceSheet";

describe("PresenceSheet", () => {
  beforeEach(() => invoke.mockClear());
  it("shows what is being authorized and its digest, and cancels the wait", async () => {
    render(<PresenceSheet />);
    expect(screen.queryByRole("dialog")).toBeNull();
    await act(async () => {
      handlers["presence://requested"]({ payload: { kind: "memberRemoved", summary: "Remove member bea", digestShort: "1a2b3c4d", op: "get" } });
    });
    expect(screen.getByText("Remove member bea")).toBeTruthy();
    expect(screen.getByTestId("presence-digest").textContent).toBe("1a2b3c4d");
    await act(async () => {
      screen.getByText("Cancel").click();
    });
    expect(invoke).toHaveBeenCalledWith("presence_cancel");
    expect(screen.queryByRole("dialog")).toBeNull();
  });
  it("closes when the browser finished", async () => {
    render(<PresenceSheet />);
    await act(async () => {
      handlers["presence://requested"]({ payload: { kind: "workspaceKeyRotated", summary: "Rotate", digestShort: "ff", op: "get" } });
    });
    await act(async () => {
      handlers["presence://done"]({ payload: true });
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
