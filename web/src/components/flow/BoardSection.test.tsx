import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { BoardView } from "./BoardSection";
import type { BoardRow } from "@/lib/flow";

const row = (patch: Partial<BoardRow>): BoardRow => ({
  id: "proposal:p",
  kind: "unreviewedProposal",
  title: "Review “Rename”",
  sessionId: "s",
  sinceMs: 0,
  stimulus: 2.4,
  chatTitle: "Parser",
  suggested: "a1",
  suggestedName: "reviewer",
  nearestThreshold: 1,
  ...patch,
});

describe("task board", () => {
  it("offers the hand-off to the suggested agent", () => {
    const picked: string[] = [];
    render(<BoardView rows={[row({})]} onPickup={(r) => picked.push(r.id)} now={2 * 3_600_000} />);
    expect(screen.getByText("Unreviewed proposal")).toBeInTheDocument();
    expect(screen.getByText("waiting 2 h")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "in “Parser”" })).toBeInTheDocument();
    expect(screen.getByText("stimulus 2.4")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Hand to reviewer"));
    expect(picked).toEqual(["proposal:p"]);
  });

  it("shows claims and uncrossed thresholds", () => {
    render(
      <BoardView
        rows={[row({ id: "a", claimedBy: "coder" }), row({ id: "b", suggested: null, suggestedName: null, nearestThreshold: 1.6 })]}
        onPickup={() => {}}
      />,
    );
    expect(screen.getByText("coder has it")).toBeInTheDocument();
    expect(screen.getByText("needs 1.6")).toBeInTheDocument();
  });

  it("is empty when nothing waits", () => {
    render(<BoardView rows={[]} onPickup={() => {}} />);
    expect(screen.getByText(/Nothing waiting/)).toBeInTheDocument();
  });

  it("shows the pickup hint once, until it's dismissed", () => {
    const { unmount } = render(<BoardView rows={[]} onPickup={() => {}} />);
    expect(screen.getByTestId("hint-flow-board")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss hint" }));
    expect(screen.queryByTestId("hint-flow-board")).not.toBeInTheDocument();
    unmount();
    render(<BoardView rows={[]} onPickup={() => {}} />);
    expect(screen.queryByTestId("hint-flow-board")).not.toBeInTheDocument();
  });
});
