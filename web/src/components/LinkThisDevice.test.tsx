import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { LinkThisDevice } from "./LinkThisDevice";
import * as ipc from "@/lib/ipc";
import type { LinkFlowDto } from "@/lib/ipc";

const flow = (phase: string, more: Partial<LinkFlowDto> = {}): LinkFlowDto => ({
  phase,
  request: null,
  fingerprint: null,
  label: null,
  code: null,
  codeSpelled: null,
  claimedName: null,
  claimedFingerprint: null,
  personName: null,
  workspaceName: null,
  workspaceId: null,
  approverFingerprint: null,
  notice: null,
  error: null,
  canRetry: false,
  ...more,
});

// The backend pushes the flow's changes as events; tests push them by hand.
let push: (v: LinkFlowDto) => void = () => {};

vi.mock("@/lib/ipc", () => ({
  linkRequestStatus: vi.fn(async () => flow("idle")),
  onLinkFlowChanged: vi.fn(async (cb: (v: LinkFlowDto) => void) => {
    push = cb;
    return () => {};
  }),
  linkRequestStart: vi.fn(async () => flow("waiting", { request: "liguslink1:REQ" })),
  linkRequestCodesMatch: vi.fn(async (m: boolean) => flow(m ? "approving" : "failed", { code: "K7Q4M9X2" })),
  linkRequestLinkAs: vi.fn(async () => flow("linking")),
  linkRequestCancel: vi.fn(async () => flow("idle")),
  linkRequestPaste: vi.fn(async () => flow("paste", { request: "liguslink1:REQ", fingerprint: "ABCD-EFGH-JKMN-PQRS-TVWX" })),
  linkRequestOpenGrant: vi.fn(async () =>
    flow("confirmApprover", { personName: "Ana", workspaceName: "Acme", approverFingerprint: "1234-5678-9ABC-DEFG-HJKM" }),
  ),
  linkRequestRedeem: vi.fn(async () => flow("linked", { personName: "Ana", workspaceId: "w1" })),
}));

const ADDRESS = "ligusaddr1:abc";
const click = (name: string | RegExp) => userEvent.click(screen.getByRole("button", { name }));
const emit = (v: LinkFlowDto) => act(() => push(v));

beforeEach(() => {
  vi.clearAllMocks();
  push = () => {};
});

async function pasteAddress() {
  await userEvent.type(screen.getByLabelText("The approval address from your other device"), ADDRESS);
  await click("Continue");
}

describe("LinkThisDevice", () => {
  it("links by matching codes: no name, then the code, then who it links as", async () => {
    const onLinked = vi.fn();
    render(<LinkThisDevice onBack={vi.fn()} onLinked={onLinked} />);
    expect(screen.queryByPlaceholderText(/name/i)).toBeNull();
    await pasteAddress();
    expect(ipc.linkRequestStart).toHaveBeenCalledWith(ADDRESS);
    await screen.findByTestId("link-waiting");

    // The other device opened it: both screens show the same code.
    emit(flow("code", { code: "K7Q4M9X2", claimedName: "Ana" }));
    expect(await screen.findByText("Does your other device show this code?")).toBeTruthy();
    expect(screen.getByTestId("match-code")).toHaveTextContent("K7Q4M9X2");
    expect(screen.getByTestId("match-code").getAttribute("aria-label")).toBe("Code: K, 7, Q, 4. M, 9, X, 2.");
    expect(screen.getByText(/Says it's Ana's device/)).toBeTruthy();
    await click("Codes match");
    expect(ipc.linkRequestCodesMatch).toHaveBeenCalledWith(true);
    await screen.findByTestId("link-approving");

    // Approved and verified on the log: this computer says who it links as.
    emit(flow("confirmPerson", { personName: "Ana", workspaceName: "Acme" }));
    expect(await screen.findByText(/Link this computer as/)).toHaveTextContent("Link this computer as Ana?");
    await click("Link");
    expect(ipc.linkRequestLinkAs).toHaveBeenCalledWith(true);
    emit(flow("linked", { personName: "Ana", workspaceId: "w1" }));
    expect(await screen.findByTestId("link-linked")).toHaveTextContent("one of Ana's devices");
    await click("Continue");
    await waitFor(() => expect(onLinked).toHaveBeenCalledWith(expect.objectContaining({ phase: "linked", workspaceId: "w1" })));
  });

  it("says when the codes don't match and offers to start again", async () => {
    render(<LinkThisDevice onBack={vi.fn()} />);
    await pasteAddress();
    emit(flow("code", { code: "K7Q4M9X2" }));
    await click(/They don.t match/);
    expect(ipc.linkRequestCodesMatch).toHaveBeenCalledWith(false);
    emit(flow("failed", { error: "The codes don't match, so nothing was granted.", canRetry: true }));
    expect(await screen.findByTestId("link-error")).toHaveTextContent("nothing was granted");
  });

  it("shows an error plainly and Try again posts again to the same address", async () => {
    render(<LinkThisDevice onBack={vi.fn()} />);
    (ipc.linkRequestStart as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      flow("failed", { error: "Couldn't reach the relay (timed out). Check the network, then try again.", canRetry: true }),
    );
    await pasteAddress();
    expect(await screen.findByTestId("link-error")).toHaveTextContent("Couldn't reach the relay");
    await click("Try again");
    await waitFor(() => expect(ipc.linkRequestStart).toHaveBeenCalledTimes(2));
    expect(ipc.linkRequestStart).toHaveBeenLastCalledWith(ADDRESS);
    await screen.findByTestId("link-waiting");
  });

  it("a final error offers no Try again", async () => {
    render(<LinkThisDevice onBack={vi.fn()} />);
    (ipc.linkRequestStart as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      flow("failed", { error: "This computer already uses Ligus as its own member (1 workspace).", canRetry: false }),
    );
    await pasteAddress();
    await screen.findByTestId("link-error");
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
    expect(screen.getByRole("button", { name: "Start over" })).toBeTruthy();
  });

  it("falls back to request codes when the relay has no mailbox", async () => {
    const onLinked = vi.fn();
    render(<LinkThisDevice onBack={vi.fn()} onLinked={onLinked} />);
    (ipc.linkRequestStart as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      flow("noMailbox", { error: "Your relay can't pass requests between devices. Use a request code instead." }),
    );
    await pasteAddress();
    expect(await screen.findByTestId("link-error")).toHaveTextContent("Use a request code instead");
    await click("Use a request code");
    expect(await screen.findByTestId("link-request-code")).toHaveTextContent("liguslink1:REQ");
    await userEvent.type(screen.getByLabelText("The code from your other device"), "ligusgrant1:GRANT");
    await click("Continue");
    expect(ipc.linkRequestOpenGrant).toHaveBeenCalledWith("ligusgrant1:GRANT");
    expect(await screen.findByText(/Approved by/)).toHaveTextContent("Approved by Ana's device for Acme.");
    await click("Yes, link");
    expect(ipc.linkRequestRedeem).toHaveBeenCalledOnce();
    await screen.findByTestId("link-linked");
    await click("Continue");
    await waitFor(() => expect(onLinked).toHaveBeenCalled());
  });

  it("picks up a request still open, but never shows a finished one again", async () => {
    (ipc.linkRequestStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce(flow("linked", { personName: "Ana" }));
    const { unmount } = render(<LinkThisDevice onBack={vi.fn()} />);
    await waitFor(() => expect(ipc.linkRequestStatus).toHaveBeenCalled());
    expect(screen.queryByTestId("link-linked")).toBeNull();
    expect(screen.getByLabelText("The approval address from your other device")).toBeTruthy();
    unmount();
    (ipc.linkRequestStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce(flow("code", { code: "K7Q4M9X2" }));
    render(<LinkThisDevice onBack={vi.fn()} />);
    expect(await screen.findByTestId("match-code")).toHaveTextContent("K7Q4M9X2");
  });

  it("a request code is there from the start, behind one quiet link", async () => {
    render(<LinkThisDevice onBack={vi.fn()} />);
    await click("Use a request code instead");
    expect(ipc.linkRequestPaste).toHaveBeenCalledOnce();
    await screen.findByTestId("link-request-code");
    expect(ipc.linkRequestStart).not.toHaveBeenCalled();
  });

  it("Back while a request is open ends it", async () => {
    const onBack = vi.fn();
    render(<LinkThisDevice onBack={onBack} />);
    await pasteAddress();
    await screen.findByTestId("link-waiting");
    await click("Back");
    expect(ipc.linkRequestCancel).toHaveBeenCalled();
    expect(onBack).toHaveBeenCalled();
  });

  it("shows a refused answer instead of failing silently", async () => {
    render(<LinkThisDevice onBack={vi.fn()} />);
    (ipc.linkRequestPaste as ReturnType<typeof vi.fn>).mockResolvedValueOnce(flow("paste", { request: "liguslink1:REQ" }));
    await click("Use a request code instead");
    (ipc.linkRequestOpenGrant as ReturnType<typeof vi.fn>).mockRejectedValueOnce("That code was made for another computer's request.");
    await userEvent.type(screen.getByLabelText("The code from your other device"), "ligusgrant1:OTHER");
    await click("Continue");
    expect(await screen.findByRole("alert")).toHaveTextContent("another computer's request");
  });
});
