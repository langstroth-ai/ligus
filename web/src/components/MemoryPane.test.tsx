import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MemoryPaneView, type Draft } from "./MemoryPane";
import { TaskStateView } from "./TaskStatePanel";
import { draftProblem, parseTags, provenance, type MemoryEntry, type MemoryView, type TaskSlotDto } from "@/lib/memory";

const ana = { id: "ana", name: "Ana", kind: "human" as const };
const entry: MemoryEntry = {
  id: "e1",
  current: { rev: "r2", base: "r1", title: "Relay ports", body: "The relay listens on 8443.", tags: ["infra"], author: ana, atMs: 1, proposer: "primary", viaProposal: "p0" },
  createdBy: ana,
  createdAtMs: 1,
  revisionCount: 2,
  conflicts: [{ rev: "r3", base: "r1", title: "Relay ports", body: "It's 8080.", author: { id: "bo", name: "Bo", kind: "human" }, atMs: 2 }],
};

function view(over: Partial<MemoryView> = {}): MemoryView {
  return {
    entries: [entry],
    total: 1,
    proposals: [
      {
        proposal: { proposalId: "p1", op: { op: "create", entryId: "e9", title: "Deploy day", body: "We ship on Thursdays." }, proposer: "a1", rationale: "said in standup", source: { chatId: "c1" } },
        by: { id: "a1", name: "coder", kind: "agent" },
        atMs: 3,
      },
    ],
    chatTitles: { c1: "Standup" },
    me: "ana",
    moderator: false,
    ...over,
  };
}

function setup(v: MemoryView) {
  const saved: Draft[] = [];
  const decided: [string, boolean][] = [];
  const removed: string[] = [];
  render(
    <MemoryPaneView
      view={v}
      query=""
      onQuery={() => {}}
      onSave={(d) => saved.push(d)}
      onRemove={(e) => removed.push(e.id)}
      onDecide={(p, a) => decided.push([p.proposal.proposalId, a])}
    />,
  );
  return { saved, decided, removed };
}

describe("memory pane", () => {
  it("shows an entry with provenance and its conflict, and resolves it", () => {
    const { saved } = setup(view());
    expect(screen.getByText("Relay ports")).toBeInTheDocument();
    expect(screen.getByText(/Ana, proposed by Ligus/)).toBeInTheDocument();
    expect(screen.getByText("1 conflict")).toBeInTheDocument();
    expect(screen.getByText("It's 8080.")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Use theirs"));
    expect(saved).toEqual([{ entryId: "e1", base: "r2", title: "Relay ports", body: "It's 8080.", tags: [], resolves: ["r3"] }]);
  });

  it("lists agent proposals and decides them", () => {
    const { decided } = setup(view());
    expect(screen.getByText("Proposed by agents (1)")).toBeInTheDocument();
    expect(screen.getByText(/by coder in “Standup”/)).toBeInTheDocument();
    expect(screen.getByText("Why: said in standup")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Reject"));
    fireEvent.click(screen.getByText("Accept"));
    expect(decided).toEqual([
      ["p1", false],
      ["p1", true],
    ]);
  });

  it("hides edit and remove from someone else's entry unless moderating", () => {
    setup(view({ me: "bo" }));
    expect(screen.queryByText("Edit")).toBeNull();
    expect(screen.queryByText("Use theirs")).toBeNull();
  });

  it("creates a note with normalised tags and refuses an empty one", () => {
    const { saved } = setup(view({ entries: [], total: 0, proposals: [] }));
    expect(screen.getByText(/No notes yet/)).toBeInTheDocument();
    fireEvent.click(screen.getByText("New note"));
    const save = screen.getByText("Save") as HTMLButtonElement;
    expect(save.closest("button")).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Note title"), { target: { value: " CI " } });
    fireEvent.change(screen.getByLabelText("Note"), { target: { value: "Flaky on macOS runners" } });
    fireEvent.change(screen.getByLabelText("Tags"), { target: { value: "CI, flaky ci" } });
    fireEvent.click(screen.getByText("Save"));
    expect(saved).toEqual([{ title: "CI", body: "Flaky on macOS runners", tags: ["ci", "flaky"] }]);
  });
});

describe("memory helpers", () => {
  it("validates drafts and parses tags", () => {
    expect(draftProblem("", "x")).toMatch(/title/);
    expect(draftProblem("a\nb", "x")).toMatch(/one line/);
    expect(draftProblem("a", "x".repeat(4001))).toMatch(/4000/);
    expect(draftProblem("a", "b")).toBeNull();
    expect(parseTags("A b,,c! a")).toEqual(["a", "b", "c"]);
    expect(provenance({ ...entry.current, proposer: undefined })).toBe("Ana");
  });
});

describe("task state", () => {
  const slot = (over: Partial<TaskSlotDto>): TaskSlotDto => ({
    update: { taskId: "t", agentKey: "a1", turnId: "u", status: "blocked", summary: "Need the schema decision", progress: 40, openQuestions: ["Keep v1?"], partialResults: ["Drafted the migration"] },
    writer: "d",
    by: ana,
    atMs: 0,
    rev: "r",
    updates: 2,
    agentName: "coder",
    active: true,
    stale: false,
    expiresAtMs: 1,
    ...over,
  });
  it("shows live slots and hides stale ones", () => {
    render(<TaskStateView slots={[slot({}), slot({ agentName: "old", stale: true, update: { ...slot({}).update, agentKey: "a2", summary: "gone" } })]} />);
    expect(screen.getByText("coder")).toBeInTheDocument();
    expect(screen.getByText("Blocked")).toBeInTheDocument();
    expect(screen.getByText("Keep v1?")).toBeInTheDocument();
    expect(screen.getByText("Drafted the migration")).toBeInTheDocument();
    expect(screen.getByLabelText("40% done")).toBeInTheDocument();
    expect(screen.queryByText("gone")).toBeNull();
  });
  it("renders nothing without slots", () => {
    const { container } = render(<TaskStateView slots={[]} />);
    expect(container.innerHTML).toBe("");
  });
});
