import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Inbox, type InboxActions } from "./Inbox";
import { needsYouItems } from "@/lib/needsYou";
import { liveTurns } from "@/state/liveTurns";
import { phoneAlerts } from "@/state/phoneAlerts";

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

  it("answers an agent's question in place", async () => {
    mount(needsYouItems({ attention: [{ sessionId: "c1", openAsks: 1, paused: false, proposals: [] }], chatTitles: titles }));
    expect(screen.getByText("1 needs you")).toBeInTheDocument();
    expect(screen.getByText("An agent asked you something")).toBeInTheDocument();
    expect(await screen.findByText("Retry cap: 30 s or 60 s?")).toBeInTheDocument();
    const ipc = await import("@/lib/ipc");
    await userEvent.click(screen.getByRole("button", { name: "30 s" }));
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", "", 0);
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
});
