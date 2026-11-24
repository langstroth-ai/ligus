import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { HomeView } from "./HomeSection";
import type { HomeReport } from "@/lib/flow";

const report: HomeReport = {
  beacon: { relayUrl: "https://relay.example", baseBranch: "main", coordinatorHostId: "h1", note: "ask Ana for an invite", updatedBy: "Ana", atMs: 0 },
  localRelayUrl: "",
  localBaseBranch: "main",
  localHostId: "me",
  coordinatorLabel: "home box",
  coordinatorOnline: true,
  mismatches: ["This device has no relay; home syncs through https://relay.example."],
};

describe("home beacon", () => {
  it("shows home, the coordinator and where this device differs", () => {
    render(<HomeView report={report} onPublish={() => {}} />);
    expect(screen.getByText("https://relay.example")).toBeInTheDocument();
    expect(screen.getByText(/home box · online/)).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("no relay");
  });

  it("publishes", () => {
    const calls: unknown[] = [];
    render(<HomeView report={{ ...report, beacon: null, mismatches: [] }} onPublish={(n, c) => calls.push([n, c])} />);
    fireEvent.click(screen.getByRole("button", { name: "Publish as home" }));
    fireEvent.click(screen.getByLabelText("This device coordinates"));
    fireEvent.click(screen.getByText("Publish home"));
    expect(calls).toEqual([["", true]]);
  });
});
