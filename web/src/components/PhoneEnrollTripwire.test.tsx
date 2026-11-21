import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { PhoneEnrollTripwire } from "./PhoneEnrollTripwire";
import * as ipc from "@/lib/ipc";
import type { PhoneEnrollTripwireDto } from "@/lib/ipc";

let emit: (e: PhoneEnrollTripwireDto) => void = () => {};

vi.mock("@/lib/ipc", () => ({
  onPhoneEnrollTripwire: vi.fn(async (cb: (e: PhoneEnrollTripwireDto) => void) => {
    emit = cb;
    return () => {};
  }),
  revokeMyDevice: vi.fn(async () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

const alert: PhoneEnrollTripwireDto = {
  rid: "rid-1",
  added: { deviceId: "added-1", label: "Ana's iPhone", fingerprint: "AAAA-BBBB-CCCC-DDDD-EEEE" },
  other: { deviceId: "other-1", label: "Ana's iPhone", fingerprint: "1111-2222-3333-4444-5555" },
};

function mount() {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <PhoneEnrollTripwire />
    </QueryClientProvider>,
  );
}

describe("the add-a-phone tripwire", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows nothing until another phone answers a code", async () => {
    mount();
    await waitFor(() => expect(ipc.onPhoneEnrollTripwire).toHaveBeenCalled());
    expect(screen.queryByTestId("phone-tripwire")).toBeNull();
  });

  it("names both phones and offers to revoke the added one", async () => {
    mount();
    await waitFor(() => expect(ipc.onPhoneEnrollTripwire).toHaveBeenCalled());
    act(() => emit(alert));
    const dlg = await screen.findByRole("alertdialog", { name: "Another phone answered this code" });
    expect(dlg).toHaveTextContent("after you added Ana's iPhone");
    expect(dlg).toHaveTextContent("AAAA-BBBB-CCCC-DDDD-EEEE");
    expect(dlg).toHaveTextContent("1111-2222-3333-4444-5555");
    await userEvent.click(screen.getByRole("button", { name: "Revoke Ana's iPhone" }));
    await waitFor(() => expect(ipc.revokeMyDevice).toHaveBeenCalledWith("added-1"));
    await waitFor(() => expect(screen.queryByTestId("phone-tripwire")).toBeNull());
  });

  it("It's mine dismisses without revoking; the same alert twice shows once", async () => {
    mount();
    await waitFor(() => expect(ipc.onPhoneEnrollTripwire).toHaveBeenCalled());
    act(() => {
      emit(alert);
      emit(alert);
    });
    await screen.findByTestId("phone-tripwire");
    await userEvent.click(screen.getByRole("button", { name: "It's mine" }));
    expect(screen.queryByTestId("phone-tripwire")).toBeNull();
    expect(ipc.revokeMyDevice).not.toHaveBeenCalled();
  });
});
