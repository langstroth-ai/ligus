import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RightRail } from "./RightRail";
import type { ProposalDto } from "@/lib/ipc";
import { alt, set } from "@/test/proposalSetFixtures";

const plain: ProposalDto = {
  id: "plain",
  title: "Bump the timeout",
  body: "",
  authorActorId: "coder",
  kind: "decision",
  status: "open",
  requiredApprovals: 1,
  qualifyingApprovals: 0,
  quorumMet: false,
  approvals: [],
  createdAt: "2026-11-01T12:00:00Z",
  dismissed: false,
  refinements: [],
};
const member: ProposalDto = { ...plain, id: "a", title: "Redis (as a plain card)", setId: "s1" };

const theSet = set({
  phase: "piping",
  frontRunner: "a",
  dial: "balanced",
  alternatives: [
    alt({
      proposalId: "a",
      label: "A",
      title: "Redis",
      status: "approved",
      support: 150,
      qualifyingScouts: 2,
      scouts: 2,
      runtimes: 2,
      meetsQuorum: true,
      qualifyingApprovals: 1,
      assessments: [
        { id: "x1", scoutName: "coder", scoutRuntime: "ollama", score: 80, currentSupport: 55, rationale: "simple", round: 0, isSelf: false, createdAt: "2026-11-13T10:00:00Z" },
        { id: "x2", scoutName: "reviewer", scoutRuntime: "copilot", score: 95, currentSupport: 95, rationale: "", round: 1, isSelf: false, createdAt: "2026-11-13T10:00:00Z" },
      ],
    }),
    alt({ proposalId: "b", label: "B", title: "In-process LRU", frozen: true, active: false, stopReason: "quorum", stoppedBy: "a", pendingScouts: ["local"] }),
  ],
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_proposals") return [plain, member];
    if (cmd === "list_proposal_sets") return [theSet];
    return [];
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

describe("proposal sets in the Review pane", () => {
  it("group alternatives under their set with quorum state, decayed scores and the next human step", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <RightRail width={400} sessionId="s1" pane="review" activeRuntimeId="" onChangePane={() => {}} onClose={() => {}} />
      </QueryClientProvider>,
    );
    const card = await screen.findByTestId("proposal-set");
    expect(within(card).getByText("Which cache?")).toBeInTheDocument();
    expect(within(card).getByText(/A is the front-runner/)).toBeInTheDocument();
    // The alternative isn't duplicated as a plain card; other proposals still are.
    expect(screen.queryByText("Redis (as a plain card)")).toBeNull();
    expect(screen.getByText("Bump the timeout")).toBeInTheDocument();

    const a = within(card).getByTestId("alternative-A");
    expect(within(a).getByText("front-runner")).toBeInTheDocument();
    expect(within(a).getByText("80 → 55")).toBeInTheDocument();
    expect(within(a).getByRole("button", { name: "Run pre-flight" })).toBeInTheDocument();
    expect(within(a).queryByRole("button", { name: "Apply" })).toBeNull();

    const b = within(card).getByTestId("alternative-B");
    expect(within(b).getByText("stopped")).toBeInTheDocument();
    // A sent scout shows as pending, never with a score.
    expect(within(b).getByTestId("pending-scout")).toHaveTextContent("assessing…");
    expect(within(b).queryAllByTestId("assessment")).toHaveLength(0);
    expect(within(b).queryByRole("button", { name: "Approve" })).toBeNull();

    expect(within(card).getByRole("radio", { name: "Balanced" })).toHaveAttribute("aria-checked", "true");
    expect(within(card).queryByRole("button", { name: "Send scouts" })).toBeNull();
    fireEvent.click(within(card).getByRole("button", { name: "Actions for “Which cache?”" }));
    expect(screen.getByRole("menuitem", { name: "Close without choosing" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "New round" })).toBeInTheDocument();
  });
});
