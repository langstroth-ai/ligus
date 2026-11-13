import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { RosterView } from "./RosterSection";
import type { RosterReport } from "@/lib/flow";

const report: RosterReport = {
  agents: [{ agentId: "a1", name: "newbie", runtimeId: "llama", career: "probation", why: "new", judged: 1, approved: 1, profile: null }],
  diversity: { agents: 2, runtimes: 1, providers: 1, monoculture: true, advice: "Every agent runs on the same runtime." },
};

describe("roster", () => {
  it("shows careers and diversity, and pins a career", () => {
    const changes: unknown[] = [];
    render(<RosterView report={report} onChange={(row, patch) => changes.push([row.name, patch])} />);
    expect(screen.getByText(/Every agent runs on the same runtime/)).toBeInTheDocument();
    expect(screen.getByText(/Probation \(low-risk work\) · new · 1\/1 approved/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Career for newbie"), { target: { value: "proposer" } });
    fireEvent.click(screen.getByLabelText("Task board for newbie"));
    expect(changes).toEqual([
      ["newbie", { career: "proposer" }],
      ["newbie", { boardOptOut: true }],
    ]);
  });
});
