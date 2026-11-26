import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApprovalSheet, HOLD_MS, COPY, titleFor } from "./ApprovalSheet";
import { spellMatchCode, groupMatchCode } from "@/components/kit";
import * as ipc from "@/lib/ipc";
import type { ApprovalDto, ApprovalSessionDto } from "@/lib/ipc";

const T0 = 1_800_000_000_000;
let clock = T0;
const now = () => clock;

const request = (over: Partial<ApprovalDto> = {}): ApprovalDto => ({
  workspaceId: "w1",
  workspaceName: "Acme",
  rid: "r1",
  kind: "box",
  label: "build-1",
  fingerprint: "7F3A-91C0-77DE-0B14-5E62",
  createdAt: T0 - 30_000,
  expiresAt: T0 + 14 * 60_000,
  relay: "https://relay.example",
  claim: { state: "unclaimed" },
  parallel: 1,
  phase: null,
  ...over,
});

const session = (over: Partial<ApprovalSessionDto> = {}): ApprovalSessionDto => ({
  rid: "r1",
  workspaceId: "w1",
  kind: "box",
  label: "build-1",
  fingerprint: "7F3A-91C0-77DE-0B14-5E62",
  relay: "https://relay.example",
  createdAt: T0 - 30_000,
  phase: "code",
  code: "K7Q4 M9X2",
  spelled: "K, 7, Q, 4. M, 9, X, 2.",
  expiresAt: T0 + 5 * 60_000,
  codeAt: T0,
  message: null,
  ...over,
});

let listing: ApprovalDto[] = [request()];
vi.mock("@/lib/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  listApprovals: vi.fn(async () => ({ requests: listing, facts: [] })),
  approvalSession: vi.fn(async () => null),
  approvalOpen: vi.fn(async () => session()),
  approvalCodesMatch: vi.fn(async () => session({ phase: "approved" })),
  approvalDontMatch: vi.fn(async () => session({ phase: "mismatch" })),
  approvalExpire: vi.fn(async () => session({ phase: "expired" })),
  approvalEnd: vi.fn(async () => {}),
  approvalForget: vi.fn(async () => {}),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() }, errMsg: (e: unknown) => String(e) }));

function mount(onClose = vi.fn()) {
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <ApprovalSheet workspaceId="w1" rid="r1" onClose={onClose} now={now} />
    </QueryClientProvider>,
  );
  return onClose;
}

describe("the code", () => {
  it("is spelled in groups, letter by letter, Crockford-lenient", () => {
    expect(spellMatchCode("K7Q4 M9X2")).toBe("K, 7, Q, 4. M, 9, X, 2.");
    expect(groupMatchCode("k7q4-m9x2")).toEqual(["K7Q4", "M9X2"]);
    expect(groupMatchCode("O1IL abcd")).toEqual(["0111", "ABCD"]);
  });

  it("titles name the label and the kind", () => {
    expect(titleFor("box")).toBe("Admit a box?");
    expect(titleFor("computer")).toBe("Add a computer to your devices?");
    expect(titleFor("phone")).toBe("Add a phone to your devices?");
  });
});

describe("ApprovalSheet", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    clock = T0;
    listing = [request()];
  });

  it("shows the code large, spelled for screen readers, with no Copy, and holds Codes match for 3 s", async () => {
    mount();
    const code = await screen.findByTestId("match-code");
    expect(code).toHaveAttribute("aria-label", "Code: K, 7, Q, 4. M, 9, X, 2.");
    expect(code.textContent).toBe("K7Q4M9X2");
    expect(code.className).toContain("text-[28px]");
    expect(screen.getByText(COPY.checkBox)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /copy/i })).toBeNull();
    const match = screen.getByRole("button", { name: "Codes match" });
    expect(match).toBeDisabled();
    expect(screen.getByRole("button", { name: "Don't match" })).toBeEnabled();
    // Still held just before 3 s.
    clock = T0 + HOLD_MS - 300;
    await act(async () => {
      await new Promise((r) => setTimeout(r, 300));
    });
    expect(match).toBeDisabled();
    clock = T0 + HOLD_MS;
    await waitFor(() => expect(match).toBeEnabled());
    await userEvent.click(match);
    await waitFor(() => expect(ipc.approvalCodesMatch).toHaveBeenCalledWith("r1"));
    expect(await screen.findByTestId("approval-approved")).toBeInTheDocument();
  });

  it("keeps the fingerprint under Details, collapsed", async () => {
    mount();
    await screen.findByTestId("match-code");
    const details = screen.getByTestId("approval-details") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(details.textContent).toContain("7F3A-91C0-77DE-0B14-5E62");
    expect(details.textContent).toContain("https://relay.example");
  });

  it("says the device of mine copy and the several-asking line", async () => {
    listing = [request({ kind: "computer", label: "lt on mbp", parallel: 2 })];
    vi.mocked(ipc.approvalOpen).mockResolvedValueOnce(session({ kind: "computer", label: "lt on mbp" }));
    mount();
    expect(await screen.findByText(COPY.checkMine)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Add a computer to your devices?" })).toBeInTheDocument();
    expect(screen.getByTestId("approval-subject")).toHaveTextContent("Computer\u201Clt on mbp\u201D");
    expect(screen.getByTestId("approval-parallel")).toHaveTextContent("2 devices are asking. Check the code on the one you're holding.");
  });

  it("shows the computer waiting to confirm between Codes match and the passkey", async () => {
    listing = [request({ kind: "computer", label: "lt on mbp" })];
    vi.mocked(ipc.approvalSession).mockResolvedValue(session({ kind: "computer", label: "lt on mbp", phase: "confirming" }));
    mount();
    expect(await screen.findByTestId("approval-confirming")).toHaveTextContent(COPY.waitingComputer);
    expect(screen.queryByRole("button", { name: "Codes match" })).toBeNull();
    expect(ipc.approvalOpen).not.toHaveBeenCalled();
    vi.mocked(ipc.approvalSession).mockResolvedValue(null);
  });

  it("a contested request offers End this request and never opens", async () => {
    listing = [request({ claim: { state: "contested", name: null } })];
    const onClose = mount();
    expect(await screen.findByTestId("approval-contested")).toHaveTextContent(COPY.contested);
    await userEvent.click(screen.getByRole("button", { name: "End this request" }));
    await waitFor(() => expect(ipc.approvalEnd).toHaveBeenCalledWith("w1", "r1"));
    expect(onClose).toHaveBeenCalled();
    expect(ipc.approvalOpen).not.toHaveBeenCalled();
  });

  it("names who else is checking it", async () => {
    listing = [request({ claim: { state: "claimedBy", name: "Ben's desktop" } })];
    mount();
    expect(await screen.findByTestId("approval-claimed")).toHaveTextContent("\u201CBen's desktop\u201D is checking this request");
    expect(ipc.approvalOpen).not.toHaveBeenCalled();
  });

  it("Don't match says nothing was granted", async () => {
    mount();
    await screen.findByTestId("match-code");
    await userEvent.click(screen.getByRole("button", { name: "Don't match" }));
    expect(await screen.findByTestId("approval-over")).toHaveTextContent("Request from \u201Cbuild-1\u201D didn't match. Nothing was granted.");
  });

  it("still warns when a device that can't approve opens it after this desktop began", async () => {
    listing = [request({ phase: "waiting", claim: { state: "contested", name: null } })];
    vi.mocked(ipc.approvalSession).mockResolvedValue(session({ phase: "waiting", code: null, codeAt: null }));
    mount();
    expect(await screen.findByTestId("approval-contested")).toHaveTextContent(COPY.contested);
    expect(screen.getByRole("button", { name: "End this request" })).toBeInTheDocument();
    vi.mocked(ipc.approvalSession).mockResolvedValue(null);
  });

  it("says when the label matches an existing device, under Details", async () => {
    listing = [request({ nameClash: true })];
    mount();
    await screen.findByTestId("match-code");
    expect(screen.getByTestId("approval-name-clash")).toHaveTextContent("This name matches an existing device.");
  });
});
