import { describe, it, expect, vi, beforeEach } from "vitest";

// confirmThen delegates to the in-app confirmDialog — window.confirm is a
// no-op in Tauri webviews (the reason the old window.confirm-based tests
// tested a contract the code deliberately abandoned). Mock the dialog and
// let each test control how the user "answers".
const mocks = vi.hoisted(() => ({ confirmDialog: vi.fn() }));
vi.mock("@/components/Dialog", () => ({ confirmDialog: mocks.confirmDialog }));

import { confirmThen, withUnverifiedRecipientConfirm } from "./confirm";

beforeEach(() => mocks.confirmDialog.mockReset());

describe("confirmThen", () => {
  it("runs the action when confirmed", async () => {
    mocks.confirmDialog.mockResolvedValue(true);
    const run = vi.fn();
    confirmThen("Sure?", run);
    await vi.waitFor(() => expect(run).toHaveBeenCalledOnce());
  });

  it("does NOT run the action when cancelled", async () => {
    mocks.confirmDialog.mockResolvedValue(false);
    const run = vi.fn();
    confirmThen("Sure?", run);
    // Flush the .then chain before asserting the negative.
    await Promise.resolve();
    await Promise.resolve();
    expect(run).not.toHaveBeenCalled();
  });

  it("passes the message and danger flag to the dialog", () => {
    mocks.confirmDialog.mockResolvedValue(false);
    confirmThen("Delete this?", () => {});
    expect(mocks.confirmDialog).toHaveBeenCalledWith("Delete this?", { danger: true });
  });
});

describe("withUnverifiedRecipientConfirm", () => {
  it("passes through when the desktop doesn't refuse", async () => {
    const call = vi.fn().mockResolvedValue("ok");
    expect(await withUnverifiedRecipientConfirm("@ana", call)).toBe("ok");
    expect(call).toHaveBeenCalledWith(false);
    expect(mocks.confirmDialog).not.toHaveBeenCalled();
  });

  it("asks, and seals only after the person confirms", async () => {
    mocks.confirmDialog.mockResolvedValue(true);
    const call = vi.fn().mockRejectedValueOnce("unverified recipient: only the relay vouches").mockResolvedValueOnce("sealed");
    expect(await withUnverifiedRecipientConfirm("@ana", call)).toBe("sealed");
    expect(call.mock.calls).toEqual([[false], [true]]);
    expect(mocks.confirmDialog.mock.calls[0][1]).toMatchObject({ title: "Unverified recipient", danger: true });
  });

  it("seals nothing when the person cancels", async () => {
    mocks.confirmDialog.mockResolvedValue(false);
    const call = vi.fn().mockRejectedValueOnce("unverified recipient: only the relay vouches");
    expect(await withUnverifiedRecipientConfirm("@ana", call)).toBeNull();
    expect(call).toHaveBeenCalledOnce();
  });

  it("rethrows other errors without asking", async () => {
    const call = vi.fn().mockRejectedValue(new Error("isn't on Ligus yet"));
    await expect(withUnverifiedRecipientConfirm("@ana", call)).rejects.toThrow("isn't on Ligus yet");
    expect(mocks.confirmDialog).not.toHaveBeenCalled();
  });
});
