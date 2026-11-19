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
});
