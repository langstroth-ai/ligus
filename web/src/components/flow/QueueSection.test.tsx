import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueueView } from "./QueueSection";
import { duration, type QueueReport } from "@/lib/flow";

const base: QueueReport = {
  gate: { open: 7, oldestWaitMs: 3 * 3_600_000, medianWaitMs: 2 * 3_600_000, trembleShare: 0.6, openedLastHour: 4, decidedLastHour: 1, overLimit: true },
  edges: [
    { edge: "gate", samples: 5, medianWaitMs: 4 * 3_600_000, trembleShare: 0.6 },
    { edge: "stage", samples: 0, medianWaitMs: 0, trembleShare: 0 },
    { edge: "handoff", samples: 2, medianWaitMs: 60_000, trembleShare: 0 },
  ],
  alert: "The review gate is the bottleneck: 7 proposal(s) waiting.",
  queueLimit: 5,
  stopUpstream: "ab",
  recruits: [{ id: "r", action: "stop_upstream", queueLen: 7, oldestWaitMs: 0, atMs: 0 }],
};

describe("queue", () => {
  it("shows the bottleneck alert, gate stats and responses", () => {
    render(<QueueView report={base} />);
    expect(screen.getByRole("alert")).toHaveTextContent("review gate is the bottleneck");
    expect(screen.getByText("7 / 5")).toBeInTheDocument();
    expect(screen.getByText("Review gate")).toBeInTheDocument();
    expect(screen.getByText(/half the chats/)).toBeInTheDocument();
    expect(screen.getByText(/told agents to stop opening new proposals/)).toBeInTheDocument();
  });

  it("says when nothing is backed up", () => {
    render(<QueueView report={{ ...base, alert: null, recruits: [] }} />);
    expect(screen.getByText("Nothing is backed up.")).toBeInTheDocument();
  });

  it("formats durations", () => {
    expect(duration(5 * 60_000)).toBe("5 min");
    expect(duration(3 * 3_600_000)).toBe("3 h");
    expect(duration(72 * 3_600_000)).toBe("3 d");
  });
});
