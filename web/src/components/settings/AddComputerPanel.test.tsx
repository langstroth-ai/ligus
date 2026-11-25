import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AddComputerPanel } from "./AddComputerPanel";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  inspectComputerRequest: vi.fn(),
  addComputer: vi.fn(),
  removeDuplicateMember: vi.fn(async () => {}),
}));
vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));
vi.mock("@/components/Dialog", () => ({ confirmDialog: vi.fn(async () => true) }));

const request = {
  deviceId: "c1",
  label: "lt on mbp",
  fingerprint: "7F3A-91C0-77DE-0B14-5E62",
  alreadyMine: false,
  revoked: false,
  someoneElses: null,
  previousMember: { memberId: "m-lt", displayName: "Alex (lt)" },
  thisDesktop: "2C41-88E0-1B9D-04AF-6E73",
};

describe("AddComputerPanel", () => {
  beforeEach(() => vi.clearAllMocks());

  it("adds only after the fingerprint is confirmed, then shows the grant and this desktop", async () => {
    vi.mocked(ipc.inspectComputerRequest).mockResolvedValue(request);
    vi.mocked(ipc.addComputer).mockResolvedValue({
      grant: "ligusgrant1:abc",
      expiresInMinutes: 30,
      thisDesktop: request.thisDesktop,
      previousMember: request.previousMember,
    });
    const onDone = vi.fn();
    render(<AddComputerPanel onDone={onDone} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText("The computer's link request"), "liguslink1:x");
    await screen.findByTestId("add-computer-request");
    expect(screen.getByTestId("add-computer-previous").textContent).toContain("Alex (lt)");
    const add = screen.getByRole("button", { name: "Add with passkey" });
    expect(add).toBeDisabled();
    await userEvent.click(screen.getByLabelText("The computer shows the same fingerprint"));
    await userEvent.click(add);
    expect(ipc.addComputer).toHaveBeenCalledWith("liguslink1:x", request.fingerprint);
    await screen.findByTestId("add-computer-grant");
    expect(screen.queryByLabelText("Grant for the computer")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Show code" }));
    expect((screen.getByLabelText("Grant for the computer") as HTMLTextAreaElement).value).toBe("ligusgrant1:abc");
    expect(screen.getByRole("img", { name: /This desktop's fingerprint/ })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Remove…" }));
    await waitFor(() => expect(ipc.removeDuplicateMember).toHaveBeenCalledWith("m-lt"));
    await userEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(onDone).toHaveBeenCalled();
  });

  it("Copy for lt copies the grant alone; Copy command and the fingerprint never carry it", async () => {
    const writeText = vi.fn(async (_text: string) => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    vi.mocked(ipc.inspectComputerRequest).mockResolvedValue({ ...request, previousMember: null });
    vi.mocked(ipc.addComputer).mockResolvedValue({
      grant: "ligusgrant1:SEALED",
      expiresInMinutes: 30,
      thisDesktop: request.thisDesktop,
      previousMember: null,
    });
    const log = vi.spyOn(console, "log");
    render(<AddComputerPanel onDone={() => {}} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText("The computer's link request"), "liguslink1:x");
    await userEvent.click(await screen.findByLabelText("The computer shows the same fingerprint"));
    await userEvent.click(screen.getByRole("button", { name: "Add with passkey" }));
    await screen.findByTestId("add-computer-grant");

    await userEvent.click(screen.getByRole("button", { name: "Copy for lt" }));
    expect(writeText).toHaveBeenLastCalledWith("ligusgrant1:SEALED");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Copy command" }));
    const command = writeText.mock.calls.at(-1)![0] as string;
    expect(command).toBe("lt --plain link-redeem --approver-fingerprint 2C41-88E0-1B9D-04AF-6E73");
    expect(command).not.toContain("ligusgrant1");

    await userEvent.click(screen.getByRole("button", { name: "Copy this desktop's fingerprint" }));
    expect(writeText).toHaveBeenLastCalledWith("2C41-88E0-1B9D-04AF-6E73");
    expect(screen.getByTestId("add-computer-how").textContent).toContain("lt --plain link-redeem");
    for (const call of log.mock.calls) expect(String(call)).not.toContain("SEALED");
  });

  it("refuses a computer that is someone else's here", async () => {
    vi.mocked(ipc.inspectComputerRequest).mockResolvedValue({ ...request, previousMember: null, someoneElses: "Taylor" });
    render(<AddComputerPanel onDone={() => {}} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText("The computer's link request"), "liguslink1:x");
    await screen.findByText(/already in the workspace as Taylor/);
    expect(screen.queryByRole("button", { name: "Add with passkey" })).toBeNull();
  });

  it("adds a computer someone else only certified, with one quiet line", async () => {
    vi.mocked(ipc.inspectComputerRequest).mockResolvedValue({ ...request, previousMember: null, alsoCertifiedBy: ["Taylor"] });
    render(<AddComputerPanel onDone={() => {}} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText("The computer's link request"), "liguslink1:x");
    expect((await screen.findByTestId("also-certified")).textContent).toContain("Taylor also certified this computer's key");
    expect(screen.getByRole("button", { name: "Add with passkey" })).toBeInTheDocument();
  });

  it("says what to paste when it isn't a link request", async () => {
    vi.mocked(ipc.inspectComputerRequest).mockRejectedValue(new Error("not a Ligus link request"));
    render(<AddComputerPanel onDone={() => {}} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText("The computer's link request"), "hivews1:x");
    expect((await screen.findByRole("alert")).textContent).toContain("ligus devices request");
  });

  it("opened from People: names the member and offers to remove it after adding", async () => {
    vi.mocked(ipc.inspectComputerRequest).mockResolvedValue({ ...request, previousMember: null });
    vi.mocked(ipc.addComputer).mockResolvedValue({
      grant: "ligusgrant1:abc",
      expiresInMinutes: 30,
      thisDesktop: request.thisDesktop,
      previousMember: null,
    });
    render(<AddComputerPanel onDone={() => {}} onCancel={() => {}} suggested={{ memberId: "m-zaz", displayName: "zaz" }} />);
    expect(screen.getByTestId("add-computer-suggested").textContent).toContain("zaz");
    await userEvent.type(screen.getByLabelText("The computer's link request"), "liguslink1:x");
    await screen.findByTestId("add-computer-request");
    await userEvent.click(screen.getByLabelText("The computer shows the same fingerprint"));
    await userEvent.click(screen.getByRole("button", { name: "Add with passkey" }));
    const row = await screen.findByTestId("add-computer-remove-old");
    expect(row.textContent).toContain("You opened this from");
    // Never removed on its own: only after the person chooses it.
    expect(ipc.removeDuplicateMember).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Remove…" }));
    await waitFor(() => expect(ipc.removeDuplicateMember).toHaveBeenCalledWith("m-zaz"));
  });

  it("prefers the member the computer proved it was", async () => {
    vi.mocked(ipc.inspectComputerRequest).mockResolvedValue(request);
    vi.mocked(ipc.addComputer).mockResolvedValue({
      grant: "ligusgrant1:abc",
      expiresInMinutes: 30,
      thisDesktop: request.thisDesktop,
      previousMember: request.previousMember,
    });
    render(<AddComputerPanel onDone={() => {}} onCancel={() => {}} suggested={{ memberId: "m-zaz", displayName: "zaz" }} />);
    await userEvent.type(screen.getByLabelText("The computer's link request"), "liguslink1:x");
    await screen.findByTestId("add-computer-request");
    await userEvent.click(screen.getByLabelText("The computer shows the same fingerprint"));
    await userEvent.click(screen.getByRole("button", { name: "Add with passkey" }));
    expect((await screen.findByTestId("add-computer-remove-old")).textContent).toContain("Alex (lt)");
    await userEvent.click(screen.getByRole("button", { name: "Remove…" }));
    await waitFor(() => expect(ipc.removeDuplicateMember).toHaveBeenCalledWith("m-lt"));
  });
});
