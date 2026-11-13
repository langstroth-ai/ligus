import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RoutingReportView } from "./RoutingSection";
import { FlowSection } from "@/components/settings/FlowSection";
import type { FlowSettings, RoutingReport } from "@/lib/flow";

const settings: FlowSettings = {
  handoffCards: true,
  routeMode: "shadow",
  epsilonMin: 0.05,
  epsilonMax: 0.3,
  halfLifeDays: 14,
  cooldownMinutes: 15,
  stopUpstream: "on",
  queueLimit: 5,
  longWaitMinutes: 120,
  recruitApprovers: false,
  offerIdleHost: false,
  marksInContext: true,
  boardAutoPickup: false,
  reuseWorktrees: true,
};

const saved: FlowSettings[] = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: { settings?: FlowSettings }) => {
    if (cmd === "flow_get_settings") return settings;
    if (cmd === "flow_set_settings") {
      saved.push(args!.settings!);
      return args!.settings;
    }
    return [];
  }),
}));

const report: RoutingReport = {
  mode: "shadow",
  sinceMs: 0,
  decisions: 4,
  offPrimaryShare: 0.5,
  scores: [
    { agent: "a", agentName: "coder", taskClass: "refactor", score: 0.82, recent: 0.6, weight: 5, outcomes: 6, successes: 5, turnsPerSuccess: 1.5 },
  ],
  arms: [{ arm: "shadow", turns: 10, proposals: 4, accepted: 3, judged: 4, acceptanceRate: 0.75, turnsPerAccepted: 3.33, costUsd: 0, costPerAccepted: null, localShare: 0.2 }],
  recent: [
    { atMs: 1, sessionId: "s", taskClass: "refactor", mode: "shadow", chosen: "Ligus (primary)", wouldPick: "coder", preferred: "coder", explore: false, reason: "sampled", epsilon: 0.06 },
  ],
  stops: [],
  exploration: { explored: 2, share: 0.12, meanEpsilon: 0.08, exploredWins: 2, exploredJudged: 3, incumbentRate: 0.5 },
};

describe("routing report", () => {
  it("explains shadow mode and shows scores, arms and decisions", () => {
    render(<RoutingReportView report={report} />);
    expect(screen.getByText(/Shadow mode/)).toBeInTheDocument();
    expect(screen.getByText("Would route off primary")).toBeInTheDocument();
    expect(screen.getByText("50%")).toBeInTheDocument();
    expect(screen.getByText("82%")).toBeInTheDocument();
    expect(screen.getByText("falling")).toBeInTheDocument();
    expect(screen.getByText("75%")).toBeInTheDocument();
    expect(screen.getByText(/would send to/)).toBeInTheDocument();
    expect(screen.getByText(/12% of decisions went to an agent other than the top scorer/)).toBeInTheDocument();
    expect(screen.getByText(/2 of 3 explored turns succeeded, against 50% for the top scorer/)).toBeInTheDocument();
  });
});

describe("route cool-downs", () => {
  it("lists stopped routes and clears one", () => {
    const cleared: string[] = [];
    const untilMs = Date.now() + 10 * 60_000;
    render(
      <RoutingReportView
        report={{ ...report, stops: [{ route: "runtime:cc", name: "every agent on cc", reason: "rate_limited", untilMs, failures: 1 }] }}
        onClearStop={(r) => cleared.push(r)}
      />,
    );
    expect(screen.getByText("every agent on cc")).toBeInTheDocument();
    expect(screen.getByText(/rate limited · 10 min left/)).toBeInTheDocument();
    fireEvent.click(screen.getByText("Clear"));
    expect(cleared).toEqual(["runtime:cc"]);
  });
});

describe("flow settings", () => {
  it("switches routing to live", async () => {
    const qc = new QueryClient();
    render(
      <QueryClientProvider client={qc}>
        <FlowSection />
      </QueryClientProvider>,
    );
    const select = await screen.findByLabelText("Routing mode");
    expect((select as HTMLSelectElement).value).toBe("shadow");
    fireEvent.change(select, { target: { value: "live" } });
    await waitFor(() => expect(saved.at(-1)?.routeMode).toBe("live"));
  });
});
