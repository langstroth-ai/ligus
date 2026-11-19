import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AddPhonePanel } from "./AddPhonePanel";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  inspectPhoneCode: vi.fn(),
  certifyPhoneCode: vi.fn(async () => "dev-1"),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

const fresh = {
  deviceId: "11111111-2222-3333-4444-555555555555",
  label: "Ana's iPhone",
  fingerprint: "ABCD-EF01-2345",
  standing: "new" as const,
  problem: null,
  workspaceId: "ws",
};

describe("Add this phone", () => {
  beforeEach(() => vi.clearAllMocks());

  it("certifies only after the fingerprint was compared, with the fingerprint shown", async () => {
    vi.mocked(ipc.inspectPhoneCode).mockResolvedValue(fresh);
    const onDone = vi.fn();
    render(<AddPhonePanel onDone={onDone} onCancel={vi.fn()} />);
    const box = screen.getByLabelText("Phone code");
    await userEvent.click(box);
    await userEvent.paste("ligusphone1:abc");
    expect(await screen.findByTestId("phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345");
    const add = screen.getByRole("button", { name: "Add this phone" });
    expect(add).toBeDisabled();
    await userEvent.click(screen.getByLabelText(/same fingerprint/));
    expect(add).toBeEnabled();
    await userEvent.click(add);
    await waitFor(() => expect(ipc.certifyPhoneCode).toHaveBeenCalledWith("ligusphone1:abc", "ABCD-EF01-2345"));
    expect(onDone).toHaveBeenCalled();
  });

  it("refuses a revoked or conflicting phone with the reason", async () => {
    vi.mocked(ipc.inspectPhoneCode).mockResolvedValue({
      ...fresh,
      standing: "conflict",
      problem: "that phone is certified here with a different key",
    });
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await userEvent.click(screen.getByLabelText("Phone code"));
    await userEvent.paste("ligusphone1:abc");
    expect(await screen.findByText(/different key/)).toBeTruthy();
    expect(screen.queryByLabelText(/same fingerprint/)).toBeNull();
    expect(screen.getByRole("button", { name: "Add this phone" })).toBeDisabled();
  });

  it("never shows (or certifies against) the answer for a code that was replaced", async () => {
    // The first paste's check answers late, after a second code was pasted.
    let late: (v: typeof fresh) => void = () => {};
    const other = { ...fresh, label: "Mallory's phone", fingerprint: "9999-9999-9999", deviceId: "99999999-9999-9999-9999-999999999999" };
    vi.mocked(ipc.inspectPhoneCode)
      .mockImplementationOnce(() => new Promise((r) => (late = r)))
      .mockResolvedValueOnce(fresh);
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    const box = screen.getByLabelText("Phone code");
    await userEvent.click(box);
    await userEvent.paste("ligusphone1:first");
    await userEvent.clear(box);
    await userEvent.paste("ligusphone1:second");
    expect(await screen.findByTestId("phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345");
    late(other);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.getByTestId("phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345");
    expect(screen.queryByText("Mallory's phone")).toBeNull();
  });
});
