import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { RightRail } from "./RightRail";
import { ArchivedProposalsSheet, archivedMeta } from "./ArchivedProposals";
import type { ProposalDto } from "@/lib/ipc";
import { OPEN_ARCHIVED_PROPOSALS_EVENT } from "@/lib/nav";

// Proposal archiving: the Review pane's row menu (Archive for everyone, Hide
// for me), its ⋯ → Archived proposals, and the workspace-wide Archived view.

const base = {
  body: "",
  authorActorId: "agent",
  kind: "decision",
  requiredApprovals: 1,
  qualifyingApprovals: 0,
  quorumMet: false,
  approvals: [],
  createdAt: "2026-11-01T12:00:00Z",
  dismissed: false,
  refinements: [],
};
const open = { ...base, id: "open", title: "Cold-day floor", status: "open" } as ProposalDto;
const applied = { ...base, id: "done", title: "Pin the weather client", status: "applied", canArchive: true, settledAt: "2026-11-02T12:00:00Z" } as ProposalDto;
const archived = { ...base, id: "old", title: "Log hive weight hourly", status: "rejected", archived: "person", dismissed: true, archivedBy: "Maya", archivedAt: "2026-11-03T12:00:00Z" } as ProposalDto;

let proposals: ProposalDto[] = [];
const calls: { cmd: string; args: unknown }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: unknown) => {
    calls.push({ cmd, args });
    if (cmd === "list_proposals") return proposals;
    if (cmd === "dismiss_proposals") return 1;
    if (cmd === "list_archived_proposals")
      return [
        { chatId: "c1", chatTitle: "Hive inspection scheduler", proposal: archived },
        { chatId: "c2", chatTitle: "Varroa mite count import", proposal: { ...open, id: "mine", title: "Sugar-roll default", hidden: true } },
      ];
    if (cmd?.startsWith("list_")) return [];
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

const client = () => new QueryClient({ defaultOptions: { queries: { retry: false } } });

function review() {
  return render(
    <QueryClientProvider client={client()}>
      <RightRail width={400} sessionId="s1" pane="review" activeRuntimeId="" onChangePane={() => {}} onClose={() => {}} />
    </QueryClientProvider>,
  );
}

function rowMenu(title: string) {
  const row = screen.getAllByTestId("proposal-row").find((r) => r.textContent?.includes(title))!;
  fireEvent.click(within(row).getByRole("button", { name: `Actions for ${title}` }));
  return screen.getByRole("menu");
}

beforeEach(() => {
  calls.length = 0;
  vi.mocked(invoke).mockClear();
});

describe("Review pane", () => {
  it("keeps archived proposals out of the pane and offers Archive only on settled ones", async () => {
    proposals = [open, applied, archived];
    review();
    await screen.findByText("Cold-day floor");
    expect(screen.queryByText("Log hive weight hourly")).toBeNull();
    expect(screen.queryByText(/dismissed/i)).toBeNull();
    // The open one: hide it for me, never archive it for everyone.
    let menu = rowMenu("Cold-day floor");
    expect(within(menu).queryByRole("menuitem", { name: "Archive" })).toBeNull();
    expect(within(menu).getByRole("menuitem", { name: "Hide for me" })).toBeTruthy();
    fireEvent.keyDown(menu, { key: "Escape" });
    // Settled: open the shelf, archive it.
    fireEvent.click(screen.getByRole("button", { name: /1 settled proposal/ }));
    menu = rowMenu("Pin the weather client");
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Archive" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "dismiss_proposals")).toBe(true));
    expect(calls.find((c) => c.cmd === "dismiss_proposals")!.args).toEqual({ sessionId: "s1", proposalIds: ["done"], dismissed: true });
  });

  it("hides for me without archiving", async () => {
    proposals = [open];
    review();
    await screen.findByText("Cold-day floor");
    fireEvent.click(within(rowMenu("Cold-day floor")).getByRole("menuitem", { name: "Hide for me" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "hide_proposals")).toBe(true));
    expect(calls.find((c) => c.cmd === "hide_proposals")!.args).toEqual({ proposalIds: ["open"], hidden: true });
    expect(calls.some((c) => c.cmd === "dismiss_proposals")).toBe(false);
  });

  it("archives the settled shelf in one go", async () => {
    proposals = [open, applied];
    review();
    fireEvent.click(await screen.findByRole("button", { name: "Archive settled" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "dismiss_proposals")).toBe(true));
    expect(calls.find((c) => c.cmd === "dismiss_proposals")!.args).toEqual({ sessionId: "s1", proposalIds: ["done"], dismissed: true });
  });

  it("opens Archived proposals from the header's ⋯", async () => {
    proposals = [open];
    review();
    await screen.findByText("Cold-day floor");
    const heard = vi.fn();
    window.addEventListener(OPEN_ARCHIVED_PROPOSALS_EVENT, heard);
    fireEvent.click(screen.getByRole("button", { name: /more/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Archived proposals" }));
    expect(heard).toHaveBeenCalled();
    window.removeEventListener(OPEN_ARCHIVED_PROPOSALS_EVENT, heard);
  });
});

describe("Archived proposals view", () => {
  function sheet(onOpen = vi.fn()) {
    render(
      <QueryClientProvider client={client()}>
        <ArchivedProposalsSheet onClose={() => {}} onOpen={onOpen} />
      </QueryClientProvider>,
    );
    return onOpen;
  }

  it("lists every chat's archived and hidden proposals, and searches them", async () => {
    sheet();
    expect(await screen.findByText("Log hive weight hourly")).toBeTruthy();
    expect(screen.getByText("Sugar-roll default")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "Search archived proposals" }), { target: { value: "varroa" } });
    expect(screen.queryByText("Log hive weight hourly")).toBeNull();
    expect(screen.getByText("Sugar-roll default")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "Search archived proposals" }), { target: { value: "nothing like it" } });
    expect(screen.getByText("No matches")).toBeTruthy();
  });

  it("restores an archived proposal for everyone, and shows a hidden one again only here", async () => {
    sheet();
    await screen.findByText("Log hive weight hourly");
    const rows = screen.getAllByTestId("archived-row");
    fireEvent.click(within(rows[0]).getByRole("button", { name: "Actions for Log hive weight hourly" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Restore" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "dismiss_proposals")).toBe(true));
    expect(calls.find((c) => c.cmd === "dismiss_proposals")!.args).toEqual({ sessionId: "c1", proposalIds: ["old"], dismissed: false });
    fireEvent.click(within(rows[1]).getByRole("button", { name: "Actions for Sugar-roll default" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Show again" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "hide_proposals")).toBe(true));
    expect(calls.find((c) => c.cmd === "hide_proposals")!.args).toEqual({ proposalIds: ["mine"], hidden: false });
  });

  it("opens a proposal in its chat with the row click", async () => {
    const onOpen = sheet();
    fireEvent.click(await screen.findByText("Log hive weight hourly"));
    expect(onOpen).toHaveBeenCalledWith("c1", "old");
  });

  it("says why each one is here in one muted line", () => {
    expect(archivedMeta({ chatId: "c", chatTitle: "Scheduler", proposal: { ...archived, archivedAt: undefined } })).toBe("Scheduler · archived by Maya");
    expect(archivedMeta({ chatId: "c", chatTitle: "Scheduler", proposal: { ...applied, archived: "auto", settledAt: undefined } })).toBe("Scheduler · auto-archived");
    expect(archivedMeta({ chatId: "c", chatTitle: "", proposal: { ...open, hidden: true } })).toBe("Untitled chat · hidden for you");
  });
});
