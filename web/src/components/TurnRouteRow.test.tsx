import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TurnRouteRow, ReplyOutcomeLine } from "./TurnRouteRow";
import * as ipc from "@/lib/ipc";
import type { TurnRouteDto } from "@/bindings/TurnRouteDto";

vi.mock("@/lib/ipc", () => ({ runTurnHere: vi.fn(async () => {}) }));

function wrap(ui: React.ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

const queued: TurnRouteDto = {
  state: "queued",
  reason: "waiting",
  text: "@coder runs on build-1, which is offline. It will answer when build-1 is back.",
  host: "build-1",
  since: "2026-11-27T10:00:00Z",
  runHere: true,
  useClaudeCode: false,
};

describe("TurnRouteRow", () => {
  beforeEach(() => vi.mocked(ipc.runTurnHere).mockClear());

  it("is one quiet line with Run here this time as the only visible action", async () => {
    wrap(<TurnRouteRow sessionId="s" messageId="m" route={queued} />);
    const row = screen.getByRole("status");
    expect(row.textContent).toContain("which is offline");
    expect(screen.getAllByRole("button")).toHaveLength(1);
    await userEvent.click(screen.getByRole("button", { name: "Run here this time" }));
    expect(ipc.runTurnHere).toHaveBeenCalledWith("s", "m", false);
  });

  it("offers nothing to run when this device may not", () => {
    wrap(<TurnRouteRow sessionId="s" messageId="m" route={{ ...queued, runHere: false }} />);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("keeps Use Claude Code instead behind the row's menu", async () => {
    const cant: TurnRouteDto = {
      ...queued,
      state: "unrunnable",
      reason: "noKeyHere",
      text: "@coder can't run here: this device has no key for 'anthropic'.",
      runHere: false,
      useClaudeCode: true,
    };
    wrap(<TurnRouteRow sessionId="s" messageId="m" route={cant} />);
    expect(screen.queryByText("Use Claude Code instead")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Turn actions" }));
    await userEvent.click(await screen.findByText("Use Claude Code instead"));
    await waitFor(() => expect(ipc.runTurnHere).toHaveBeenCalledWith("s", "m", true));
  });
});

describe("ReplyOutcomeLine", () => {
  it("says Failed or Canceled, and nothing for a posted reply", () => {
    const { rerender, container } = render(<ReplyOutcomeLine outcome={{ status: "ok", harness: null, model: "opus", host: null }} />);
    expect(container.textContent).toBe("");
    rerender(<ReplyOutcomeLine outcome={{ status: "canceled", harness: null, model: null, host: null }} />);
    expect(screen.getByRole("note").textContent).toBe("Canceled");
    rerender(<ReplyOutcomeLine outcome={{ status: "failed", harness: "claudeCode", model: "opus", host: "build-1" }} />);
    expect(screen.getByRole("note").textContent).toBe("Failed · opus on build-1");
  });
});
