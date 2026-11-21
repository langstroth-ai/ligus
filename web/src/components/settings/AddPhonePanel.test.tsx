import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AddPhonePanel, POLL_MS, spellFingerprint } from "./AddPhonePanel";
import * as ipc from "@/lib/ipc";
import { toast } from "@/components/Toast";

vi.mock("@/lib/ipc", () => ({
  inspectPhoneCode: vi.fn(),
  certifyPhoneCode: vi.fn(async () => "dev-1"),
  phoneEnrollPrecheck: vi.fn(),
  phoneEnrollOfferStart: vi.fn(),
  phoneEnrollOfferShow: vi.fn(),
  phoneEnrollOfferHide: vi.fn(async () => {}),
  phoneEnrollOfferCancel: vi.fn(async () => {}),
  phoneEnrollOfferPoll: vi.fn(),
  phoneEnrollOfferApprove: vi.fn(),
  phoneEnrollOfferDecline: vi.fn(async () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

const fresh = {
  deviceId: "11111111-2222-3333-4444-555555555555",
  label: "Ana's iPhone",
  fingerprint: "ABCD-EF01-2345-6789-ABCD",
  standing: "new" as const,
  problem: null,
  workspaceId: "ws",
};

const account = { displayName: "Ana", role: "Owner", workspaceName: "acme", nameCollision: false };
const SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><path d="M4,4h1v1h-1z"/></svg>';

const offer = (ttlS = 300) => ({
  qrSvg: SVG,
  rid: "rid-1",
  expiresAt: Math.floor(Date.now() / 1000) + ttlS,
  account,
});

const phone = {
  deviceId: "11111111-2222-3333-4444-555555555555",
  label: "Ana's iPhone",
  fingerprint: "ABCD-EF01-2345-6789-ABCD",
  standing: "new" as const,
  problem: null,
  via: "Home box",
  duplicateLabel: false,
};

const waiting = { state: "waiting" as const, rid: "rid-1", expiresAt: 0, phone: null, holdRemainingMs: null };
const request = (holdRemainingMs: number, p = phone) => ({
  state: "request" as const,
  rid: "rid-1",
  expiresAt: 0,
  phone: p,
  holdRemainingMs,
});

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe("Add a phone (the phone scans the desktop's code)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(ipc.phoneEnrollPrecheck).mockResolvedValue({ ready: true, missing: [] });
    vi.mocked(ipc.phoneEnrollOfferStart).mockImplementation(async () => offer());
    vi.mocked(ipc.phoneEnrollOfferShow).mockImplementation(async () => offer());
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue(waiting);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const user = () => userEvent.setup({ advanceTimers: vi.advanceTimersByTime });

  it("draws no code until Show code, then shows it as a labelled image with no text", async () => {
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    const show = await screen.findByRole("button", { name: "Show code" });
    expect(ipc.phoneEnrollOfferStart).not.toHaveBeenCalled();
    expect(screen.queryByTestId("add-phone-qr")).toBeNull();
    await user().click(show);
    const qr = await screen.findByRole("img", { name: /QR code to add a phone/ });
    expect(qr.querySelector("svg")).toBeTruthy();
    expect(qr.textContent).toBe("");
    expect(screen.getByText(/Settings → This phone → Add to my devices/)).toBeTruthy();
    // The old hint that sent people to the phone's camera app is gone.
    expect(screen.queryByText(/camera app/)).toBeNull();
  });

  it("hides the code when the window loses focus and brings the same offer back", async () => {
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    await screen.findByTestId("add-phone-qr");
    act(() => {
      fireEvent.blur(window);
    });
    expect(screen.queryByTestId("add-phone-qr")).toBeNull();
    expect(ipc.phoneEnrollOfferHide).toHaveBeenCalled();
    expect(screen.getByTestId("add-phone-hidden")).toHaveTextContent(/hidden while Ligus isn't in front/);
    await user().click(screen.getByRole("button", { name: "Show code" }));
    expect(ipc.phoneEnrollOfferShow).toHaveBeenCalled();
    expect(ipc.phoneEnrollOfferStart).toHaveBeenCalledTimes(1);
    await screen.findByTestId("add-phone-qr");
  });

  it("counts down and announces the last minute and the expiry, not every second", async () => {
    vi.mocked(ipc.phoneEnrollOfferStart).mockImplementation(async () => offer(62));
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    expect(screen.getByTestId("add-phone-countdown")).toHaveTextContent("Expires in 1:02");
    expect(screen.getByTestId("add-phone-countdown")).toHaveAttribute("aria-hidden");
    expect(screen.getByTestId("add-phone-expiry-text")).toHaveTextContent(/The code expires at/);
    const live = screen.getByTestId("add-phone-announce");
    expect(live).toHaveAttribute("aria-live", "polite");
    expect(live).toHaveTextContent("");
    await advance(3_000);
    expect(live).toHaveTextContent("One minute left to scan the code.");
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue({ ...waiting, state: "expired" });
    await advance(POLL_MS);
    expect(await screen.findByTestId("add-phone-expired")).toHaveTextContent("This code expired.");
    expect(live).toHaveTextContent("The code expired.");
    expect(screen.getByRole("button", { name: "Show a new code" })).toBeTruthy();
  });

  it("holds Add this phone for 5 seconds after the phone appears, then certifies with the fingerprint", async () => {
    const onDone = vi.fn();
    vi.mocked(ipc.phoneEnrollOfferApprove).mockResolvedValue({
      deviceId: phone.deviceId,
      label: "Ana's iPhone",
      watchingUntil: Math.floor(Date.now() / 1000) + 300,
    });
    render(<AddPhonePanel onDone={onDone} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    // The app counts the hold from when the phone first appeared.
    const seen = Date.now() + POLL_MS;
    vi.mocked(ipc.phoneEnrollOfferPoll).mockImplementation(async () => request(Math.max(0, seen + 5_000 - Date.now())));
    await advance(POLL_MS);
    const card = await screen.findByTestId("add-phone-request");
    expect(card).toHaveTextContent("Ana's iPhone wants to be added");
    expect(card).toHaveTextContent("Relayed by Home box");
    expect(screen.getByTestId("add-phone-account")).toHaveTextContent("It will act as you, Ana (Owner), in acme");
    expect(screen.getByTestId("phone-name-hint")).toHaveTextContent(/name the phone gives itself/);
    expect(screen.getByTestId("phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345-6789-ABCD");
    expect(screen.getByTestId("phone-fingerprint")).toHaveAttribute("aria-label", spellFingerprint(phone.fingerprint));
    // The QR left the screen once the phone answered.
    expect(screen.queryByTestId("add-phone-qr")).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(card));
    const add = screen.getByRole("button", { name: "Add this phone" });
    await user().click(screen.getByLabelText(/Your phone shows the same fingerprint/));
    expect(add).toBeDisabled();
    expect(screen.getByTestId("add-phone-hold")).toHaveTextContent(/Listening for other phones/);
    await advance(2_000);
    expect(add).toBeDisabled();
    await advance(3_500);
    expect(add).toBeEnabled();
    expect(screen.queryByTestId("add-phone-hold")).toBeNull();
    await user().click(add);
    await waitFor(() => expect(ipc.phoneEnrollOfferApprove).toHaveBeenCalledWith("rid-1", "ABCD-EF01-2345-6789-ABCD"));
    expect(onDone).toHaveBeenCalled();
    expect(vi.mocked(toast.success).mock.calls[0][0]).toMatch(/another phone answers the same code/);
  });

  it("can't add before the fingerprint was compared", async () => {
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue(request(0));
    await advance(POLL_MS);
    await screen.findByTestId("add-phone-request");
    expect(screen.getByRole("button", { name: "Add this phone" })).toBeDisabled();
  });

  it("flags a duplicate name and another member with your name", async () => {
    vi.mocked(ipc.phoneEnrollOfferStart).mockImplementation(async () => ({
      ...offer(),
      account: { ...account, nameCollision: true },
    }));
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue(request(0, { ...phone, duplicateLabel: true }));
    await advance(POLL_MS);
    expect(await screen.findByTestId("add-phone-name-collision")).toHaveTextContent(/also called Ana/);
    expect(screen.getByTestId("add-phone-duplicate")).toHaveTextContent(/already have a device called Ana's iPhone/);
  });

  it("Not my phone declines the request and closes the code", async () => {
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue(request(5_000));
    await advance(POLL_MS);
    await user().click(await screen.findByRole("button", { name: "Not my phone" }));
    expect(ipc.phoneEnrollOfferDecline).toHaveBeenCalledWith("rid-1");
    expect(await screen.findByRole("button", { name: "Show code" })).toBeTruthy();
  });

  it("a phone that can't be added shows why and offers only Close", async () => {
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue(
      request(0, { ...phone, standing: "revoked", problem: "This phone was revoked here." } as never),
    );
    await advance(POLL_MS);
    expect(await screen.findByText(/revoked here/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Add this phone" })).toBeNull();
    await user().click(screen.getByRole("button", { name: "Close" }));
    expect(ipc.phoneEnrollOfferDecline).toHaveBeenCalledWith("rid-1");
  });

  it("two phones answering one code stop the flow with an alert", async () => {
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue(request(5_000));
    await advance(POLL_MS);
    await screen.findByTestId("add-phone-request");
    vi.mocked(ipc.phoneEnrollOfferPoll).mockResolvedValue({ ...waiting, state: "aborted" });
    await advance(POLL_MS);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Two phones answered this code.");
    expect(alert).toHaveTextContent(/Nothing was added/);
    expect(screen.queryByRole("button", { name: "Add this phone" })).toBeNull();
    await user().click(screen.getByRole("button", { name: "Show a new code" }));
    expect(ipc.phoneEnrollOfferStart).toHaveBeenCalledTimes(2);
  });

  it("closing the panel drops the offer", async () => {
    const onCancel = vi.fn();
    render(<AddPhonePanel onDone={vi.fn()} onCancel={onCancel} />);
    await user().click(await screen.findByRole("button", { name: "Show code" }));
    await user().click(screen.getByRole("button", { name: "Cancel" }));
    expect(ipc.phoneEnrollOfferCancel).toHaveBeenCalled();
    expect(onCancel).toHaveBeenCalled();
  });

  it("opens on the paste flow and says why when this desktop can't finish", async () => {
    vi.mocked(ipc.phoneEnrollPrecheck).mockResolvedValue({
      ready: false,
      missing: ["Adding a phone needs your passkey: add a passkey (or a recovery key) under Settings → Passkeys & keys first."],
    });
    render(<AddPhonePanel onDone={vi.fn()} onCancel={vi.fn()} />);
    expect(await screen.findByTestId("add-phone-unavailable")).toHaveTextContent(/needs your passkey/);
    expect(screen.queryByRole("button", { name: "Show code" })).toBeNull();
    expect(screen.getByLabelText("Phone code")).toBeTruthy();
  });
});

describe("Can't scan? (paste the phone's code)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(ipc.phoneEnrollPrecheck).mockResolvedValue({ ready: true, missing: [] });
  });

  async function openPaste() {
    render(<AddPhonePanel onDone={onDone} onCancel={vi.fn()} />);
    await userEvent.click(await screen.findByRole("button", { name: "Can't scan?" }));
  }
  let onDone = vi.fn();

  it("certifies only after the fingerprint was compared, with the fingerprint shown", async () => {
    onDone = vi.fn();
    vi.mocked(ipc.inspectPhoneCode).mockResolvedValue(fresh);
    await openPaste();
    expect(screen.queryByText(/camera app/)).toBeNull();
    const box = screen.getByLabelText("Phone code");
    await userEvent.click(box);
    await userEvent.paste("ligusphone1:abc");
    expect(await screen.findByTestId("paste-phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345-6789-ABCD");
    const add = screen.getByRole("button", { name: "Add this phone" });
    expect(add).toBeDisabled();
    await userEvent.click(screen.getByLabelText(/same fingerprint/));
    expect(add).toBeEnabled();
    await userEvent.click(add);
    await waitFor(() => expect(ipc.certifyPhoneCode).toHaveBeenCalledWith("ligusphone1:abc", "ABCD-EF01-2345-6789-ABCD", "ws"));
    expect(onDone).toHaveBeenCalled();
    // The phone can't know it was added: say what to do next on it.
    expect(vi.mocked(toast.success).mock.calls[0][0]).toMatch(/tap Retry on the phone/);
    expect(vi.mocked(toast.success).mock.calls[0][0]).toMatch(/ligus pair/);
  });

  it("marks the name as the phone's own claim, next to the fingerprint", async () => {
    vi.mocked(ipc.inspectPhoneCode).mockResolvedValue({ ...fresh, label: "iPhone" });
    await openPaste();
    await userEvent.click(screen.getByLabelText("Phone code"));
    await userEvent.paste("ligusphone1:abc");
    expect(await screen.findByTestId("paste-phone-name-hint")).toHaveTextContent(/name the phone gives itself/);
  });

  it("refuses a revoked or conflicting phone with the reason", async () => {
    vi.mocked(ipc.inspectPhoneCode).mockResolvedValue({
      ...fresh,
      standing: "conflict",
      problem: "that phone is certified here with a different key",
    });
    await openPaste();
    await userEvent.click(screen.getByLabelText("Phone code"));
    await userEvent.paste("ligusphone1:abc");
    expect(await screen.findByText(/different key/)).toBeTruthy();
    expect(screen.queryByLabelText(/same fingerprint/)).toBeNull();
    expect(screen.getByRole("button", { name: "Add this phone" })).toBeDisabled();
  });

  it("never shows (or certifies against) the answer for a code that was replaced", async () => {
    // The first paste's check answers late, after a second code was pasted.
    let late: (v: typeof fresh) => void = () => {};
    const other = { ...fresh, label: "Mallory's phone", fingerprint: "9999-9999-9999-9999-9999", deviceId: "99999999-9999-9999-9999-999999999999" };
    vi.mocked(ipc.inspectPhoneCode)
      .mockImplementationOnce(() => new Promise((r) => (late = r)))
      .mockResolvedValueOnce(fresh);
    await openPaste();
    const box = screen.getByLabelText("Phone code");
    await userEvent.click(box);
    await userEvent.paste("ligusphone1:first");
    await userEvent.clear(box);
    await userEvent.paste("ligusphone1:second");
    expect(await screen.findByTestId("paste-phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345-6789-ABCD");
    late(other);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.getByTestId("paste-phone-fingerprint")).toHaveTextContent("ABCD-EF01-2345-6789-ABCD");
    expect(screen.queryByText("Mallory's phone")).toBeNull();
  });
});

describe("spellFingerprint", () => {
  it("reads the fingerprint out group by group", () => {
    expect(spellFingerprint("AB12-CD34")).toBe("Fingerprint A B 1 2, C D 3 4");
  });
});
