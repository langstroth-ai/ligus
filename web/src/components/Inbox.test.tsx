import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Inbox, type InboxActions } from "./Inbox";
import { needsYouItems } from "@/lib/needsYou";
import { liveTurns } from "@/state/liveTurns";
import { phoneAlerts } from "@/state/phoneAlerts";
import { approvalSheet } from "@/state/approvals";

vi.mock("@/lib/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  listWorkspaceRepos: vi.fn(async () => ({ repos: [], canAdmin: true, suggestion: null })),
  listAsks: vi.fn(async () => [
    { askId: "a1", agentName: "scout", question: "Retry cap: 30 s or 60 s?", options: ["30 s", "60 s"], expiresAtMs: Date.now() + 60_000, open: true },
  ]),
  answerAsk: vi.fn(async () => {}),
  listPendingInvites: vi.fn(async () => ({
    contacts: "on",
    invites: [{ relay: "r", seq: 7, isDm: false, workspaceName: "Hive scale data", founderName: "Maya", trust: "known", safetyCode: "b7e1 04aa" }],
  })),
  acceptPendingInvite: vi.fn(async () => ({ name: "Hive scale data" })),
  dismissPendingInvite: vi.fn(async () => {}),
  getSandboxPolicy: vi.fn(async () => ({ changed: false, appliesHere: true, grants: {} })),
  ackSandboxGrants: vi.fn(async () => {}),
  getWorkspaceTrust: vi.fn(async () => ({ root: "/r", items: [] })),
}));

function mount(items: ReturnType<typeof needsYouItems>, actions?: Partial<InboxActions>) {
  const a: InboxActions = { openChat: vi.fn(), openProposal: vi.fn(), openSettingsTab: vi.fn(), ...actions };
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <Inbox items={items} actions={a} />
    </QueryClientProvider>,
  );
  return a;
}

const titles = new Map([["c1", "Relay backoff"]]);

describe("Inbox", () => {
  beforeEach(() => {
    liveTurns._reset();
    phoneAlerts._reset();
  });

  it("says when nothing needs you", () => {
    mount([]);
    expect(screen.getByText("Nothing needs you.")).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Needs you" })).toBeNull();
  });

  it("lists each question you can answer, with no answer buttons, and opens its chat", async () => {
    const a = mount(
      needsYouItems({
        attention: [
          {
            sessionId: "c1",
            openAsks: 1,
            paused: false,
            proposals: [],
            asks: [
              { askId: "a1", agentName: "scout", kind: "question", eligible: true, audience: "you" },
              { askId: "a2", agentName: "builder", kind: "question", eligible: false, audience: "Admins" },
            ],
          },
        ],
        chatTitles: titles,
      }),
    );
    expect(screen.getByText("1 needs you")).toBeInTheDocument();
    expect(screen.getByText("scout asks you")).toBeInTheDocument();
    expect(screen.queryByText("builder asks Admins")).toBeNull();
    const list = screen.getByRole("list", { name: "Needs you" });
    expect(within(list).queryByRole("button", { name: "30 s" })).toBeNull();
    expect(screen.queryByTestId("ask-card")).toBeNull();
    await userEvent.click(screen.getByText("scout asks you"));
    expect(a.openChat).toHaveBeenCalledWith("c1");
  });

  it("shows questions put to others only when nothing else waits", () => {
    mount(
      needsYouItems({
        attention: [{ sessionId: "c1", openAsks: 0, paused: false, proposals: [], asks: [{ askId: "a2", agentName: "builder", kind: "question", eligible: false, audience: "Admins" }] }],
        chatTitles: titles,
      }),
    );
    expect(screen.getByText("builder asks Admins")).toBeInTheDocument();
    expect(screen.getByText("Nothing needs you.")).toBeInTheDocument();
  });

  it("sends a vote to the change, never approving from the list", async () => {
    const a = mount(
      needsYouItems({
        attention: [{ sessionId: "c1", openAsks: 0, paused: false, proposals: [{ id: "p1", title: "Cold-day floor", status: "open", needsMe: true }] }],
        chatTitles: titles,
      }),
    );
    const list = screen.getByRole("list", { name: "Needs you" });
    expect(within(list).queryByRole("button", { name: /approve/i })).toBeNull();
    await userEvent.click(within(list).getByRole("button", { name: "Review" }));
    expect(a.openProposal).toHaveBeenCalledWith("c1", "p1");
  });

  it("a failed run opens its chat or is dismissed", async () => {
    liveTurns.onStream({ sessionId: "c1", messageId: "m1", phase: "delta", text: "" });
    liveTurns.onStream({ sessionId: "c1", messageId: "m1", phase: "error", text: "the model refused" });
    const items = needsYouItems({ live: liveTurns.snapshot(), chatTitles: titles });
    mount(items);
    expect(screen.getByText("the model refused")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /^Actions for / }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Dismiss" }));
    expect(liveTurns.failureOf("c1")).toBeNull();
  });

  it("reopens a phone alert that was put aside", async () => {
    phoneAlerts.add({
      rid: "r1",
      added: { deviceId: "d1", label: "Ana's iPhone", fingerprint: "AAAA" },
      other: { deviceId: "d2", label: "iPhone", fingerprint: "BBBB" },
    });
    phoneAlerts.close();
    mount(needsYouItems({ phoneAlerts: [{ rid: "r1", otherDeviceId: "d2", addedLabel: "Ana's iPhone" }] }));
    await userEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(phoneAlerts.snapshot().open).toBe(true);
  });

  it("lists invites with their safety code, and a box asking to be your device", async () => {
    const a = mount(
      needsYouItems({
        invites: [{ relay: "r", seq: 7, isDm: false, workspaceName: "Hive scale data", founderName: "Maya" }],
        deviceRequests: [{ hostId: "h1", label: "serve box" }],
      }),
    );
    expect(await screen.findByText("Join Hive scale data (founded by Maya)")).toBeInTheDocument();
    expect(screen.getByText("b7e1 04aa")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Accept" })).toBeInTheDocument();
    const row = screen.getByText("serve box says it's your device").closest('[role="listitem"]') as HTMLElement;
    await userEvent.click(within(row).getByRole("button", { name: "Review" }));
    expect(a.openSettingsTab).toHaveBeenCalledWith("account");
  });

  it("an approval request is one line that opens the sheet, with nothing to approve on the row", async () => {
    mount(needsYouItems({ approvals: [{ workspaceId: "w1", rid: "r1", kind: "box", label: "build-1", createdAt: Date.now() - 120_000 }] }));
    const row = screen.getByText("A box asks to join: \u201Cbuild-1\u201D").closest('[role="listitem"]') as HTMLElement;
    expect(within(row).getByText("2m")).toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: /approve|codes match|admit/i })).toBeNull();
    await userEvent.click(screen.getByText("A box asks to join: \u201Cbuild-1\u201D"));
    expect(approvalSheet.snapshot()).toEqual({ workspaceId: "w1", rid: "r1" });
    approvalSheet.close();
  });

  it("collapses more than three requests and expands them on click", async () => {
    const reqs = ["b1", "b2", "b3", "b4"].map((l, i) => ({ workspaceId: "w1", rid: `r${i}`, kind: "box" as const, label: l, createdAt: Date.now() - i }));
    mount(needsYouItems({ approvals: reqs }));
    expect(screen.getByText("4 need you")).toBeInTheDocument();
    expect(screen.queryByText("A box asks to join: \u201Cb1\u201D")).toBeNull();
    await userEvent.click(screen.getByText("4 requests to approve"));
    expect(screen.getByText("A box asks to join: \u201Cb4\u201D")).toBeInTheDocument();
  });

  it("shows a phone request read-only, pointing at Add a phone", async () => {
    const a = mount(needsYouItems({ approvals: [{ workspaceId: "w1", rid: "p1", kind: "phone", label: "Ana's iPhone", createdAt: Date.now() }] }));
    expect(screen.getByText("Add phones from My devices > Add a phone.")).toBeInTheDocument();
    await userEvent.click(screen.getByText("A phone asks to be your device: \u201CAna's iPhone\u201D"));
    expect(approvalSheet.snapshot()).toBeNull();
    expect(a.openSettingsTab).toHaveBeenCalledWith("account");
  });

  it("keeps a mismatch fact with a ? explaining it", () => {
    mount(needsYouItems({ approvalFacts: [{ key: "mismatch:r1", kind: "mismatch", label: "build-1", at: Date.now() }] }));
    const row = screen.getByText("Request from \u201Cbuild-1\u201D didn't match. Nothing was granted.").closest('[role="listitem"]') as HTMLElement;
    expect(within(row).getByRole("button", { name: "About a request that didn't match" })).toBeInTheDocument();
    expect(screen.getByText("Nothing needs you.")).toBeInTheDocument();
  });

  it("shows a passkey hand-off with what it does, opening the sheet", async () => {
    const summary = "Define workspace runtime \u201Ccoder\u201D (ClaudeCode, model \u201Csonnet\u201D)";
    mount(needsYouItems({ approvals: [{ workspaceId: "w1", rid: "h1", kind: "presence", label: "laptop", createdAt: Date.now(), summary }] }));
    expect(screen.getByText(summary)).toBeInTheDocument();
    await userEvent.click(screen.getByText("Your passkey, asked by your device: \u201Claptop\u201D"));
    expect(approvalSheet.snapshot()).toEqual({ workspaceId: "w1", rid: "h1" });
    approvalSheet.close();
  });

  it("keeps a Not me fact with My devices to revoke the device", async () => {
    const a = mount(needsYouItems({ approvalFacts: [{ key: "h1", kind: "notMe", label: "laptop", at: Date.now(), help: "Not me revokes nothing by itself." }] }));
    const row = screen.getByText("You said Not me to a request from \u201Claptop\u201D. Nothing was written.").closest('[role="listitem"]') as HTMLElement;
    await userEvent.click(within(row).getByRole("button", { name: "My devices" }));
    expect(a.openSettingsTab).toHaveBeenCalledWith("account");
  });
});
