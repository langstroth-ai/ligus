import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

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
  it("confirms with the recovery key words instead of a passkey", async () => {
    render(<PresenceSheet />);
    await act(async () => {
      handlers["presence://requested"]({ payload: { kind: "memberRemoved", summary: "Remove member bea", digestShort: "1a2b3c4d", op: "get" } });
    });
    await act(async () => {
      screen.getByText("Use recovery key").click();
    });
    const box = screen.getByLabelText("Recovery key words") as HTMLTextAreaElement;
    await act(async () => {
      fireEvent.change(box, { target: { value: "abandon ability able" } });
    });
    await act(async () => {
      screen.getByText("Confirm with recovery key").click();
    });
    // Bound to the digest the sheet shows (the desktop drops words for another request).
    expect(invoke).toHaveBeenCalledWith("presence_use_recovery", { words: "abandon ability able", digest: "1a2b3c4d" });
  });
  it("names the system dialog on the native path and switches to the browser", async () => {
    render(<PresenceSheet />);
    await act(async () => {
      handlers["presence://requested"]({
        payload: { kind: "memberRemoved", summary: "Remove member bea", digestShort: "1a2b", op: "get", via: "native" },
      });
    });
    expect(screen.getByTestId("presence-native").textContent).toContain("system dialog");
    expect(screen.queryByText(/Your browser opened/)).toBeNull();
    await act(async () => {
      screen.getByText("Use the browser instead").click();
    });
    expect(invoke).toHaveBeenCalledWith("presence_use_browser");
    // The hand-off then takes over the same sheet.
    await act(async () => {
      handlers["presence://requested"]({
        payload: { kind: "memberRemoved", summary: "Remove member bea", digestShort: "1a2b", op: "get", via: "browser" },
      });
    });
    expect(screen.getByText(/Your browser opened/)).toBeTruthy();
    expect(screen.queryByText("Use the browser instead")).toBeNull();
  });
  it("after the system dialog is closed, offers the browser, the recovery key and Cancel", async () => {
    render(<PresenceSheet />);
    await act(async () => {
      handlers["presence://requested"]({
        payload: { kind: "memberRemoved", summary: "Remove member bea", digestShort: "1a2b", op: "get", via: "dismissed" },
      });
    });
    expect(screen.getByTestId("presence-native").textContent).toContain("closed the system passkey dialog");
    expect(screen.getByText("Use the browser instead")).toBeTruthy();
    expect(screen.getByText("Use recovery key")).toBeTruthy();
    expect(screen.getByText("Cancel")).toBeTruthy();
  });
  it("offers no recovery key while creating a passkey", async () => {
    render(<PresenceSheet />);
    await act(async () => {
      handlers["presence://requested"]({ payload: { kind: "passkeyRegistered", summary: "Create", digestShort: "aa", op: "create" } });
    });
    expect(screen.queryByText("Use recovery key")).toBeNull();
  });
});
