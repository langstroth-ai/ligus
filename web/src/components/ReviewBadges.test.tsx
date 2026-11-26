import { describe, it, expect, vi } from "vitest";
import { render, screen, within, fireEvent } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ProposalBadges } from "./ReviewBadges";
import { PlanBody } from "./ImplementPreview";
import { AgentAsks } from "./AgentAsks";
import type { ImplementPreviewDto, ProposalDto } from "@/lib/ipc";

const base: ProposalDto = {
  id: "p1",
  title: "Run the tests",
  body: "",
  authorActorId: "a1",
  kind: "command",
  status: "open",
  requiredApprovals: 1,
  qualifyingApprovals: 0,
  quorumMet: false,
  approvals: [],
  createdAt: "2026-11-14T12:00:00Z",
  dismissed: false,
  refinements: [],
};

const invoke = vi.fn(async (cmd: string, _args?: unknown) => {
  if (cmd === "list_asks")
    return [
      { askId: "q1", agentName: "@coder", question: "Which database?", options: ["sqlite", "postgres"], expiresAtMs: 0, open: true, live: true, chatId: "s1", kind: "question", v: 2, allowText: false, audience: { kind: "room", minRole: "contributor" }, quorum: 1, eligible: true, tally: [0, 0], declined: false, answeredBy: [], olderAnswerUncounted: false, atMs: 0 },
      { askId: "q0", agentName: "@coder", question: "Old one", options: [], expiresAtMs: 0, open: false, live: false, answer: "done", chatId: "s1", kind: "question", v: 2, allowText: false, audience: { kind: "room", minRole: "contributor" }, quorum: 1, eligible: true, tally: [0, 0], declined: false, answeredBy: [], olderAnswerUncounted: false, atMs: 0 },
    ];
  return null;
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

describe("review badges", () => {
  it("show kind, an agent author with its signature, config impact, gates and a legacy command", () => {
    render(
      <ProposalBadges
        proposal={{
          ...base,
          authorKind: "agent",
          authorName: "@coder",
          signedBy: "agent",
          configImpact: true,
          humanGate: true,
          presenceRequired: true,
          legacyCommand: true,
        }}
      />,
    );
    const b = screen.getByTestId("proposal-badges");
    expect(within(b).getByText("Command")).toBeInTheDocument();
    expect(within(b).getByText(/@coder · agent-signed/)).toBeInTheDocument();
    expect(within(b).getByText("Config impact")).toBeInTheDocument();
    expect(within(b).getByText("Human gate")).toBeInTheDocument();
    expect(within(b).getByText("Presence required")).toBeInTheDocument();
    expect(within(b).getByText("No exact command")).toBeInTheDocument();
  });

  it("mark host-signed agent output as legacy and stay quiet for a plain human decision", () => {
    const { rerender } = render(<ProposalBadges proposal={{ ...base, kind: "fileDiff", authorKind: "agent", authorName: "@coder", signedBy: "host" }} />);
    expect(screen.getByText(/host-signed \(legacy\)/)).toBeInTheDocument();
    expect(screen.getByText("File change")).toBeInTheDocument();
    rerender(<ProposalBadges proposal={{ ...base, kind: "decision", authorKind: "human", authorName: "Mara", signedBy: "human" }} />);
    const b = screen.getByTestId("proposal-badges");
    expect(within(b).getByText("Decision")).toBeInTheDocument();
    expect(within(b).getByText("Mara")).toBeInTheDocument();
    expect(within(b).queryByText("Config impact")).toBeNull();
    expect(within(b).queryByText(/signed/)).toBeNull();
  });
});

describe("implement preview", () => {
  const plan: ImplementPreviewDto = {
    kind: "argv",
    argv: ["cargo", "test", "--", "it's"],
    cwd: "crates",
    targetAgentId: "a1",
    targetName: "@coder",
    targets: [["a1", "@coder"], ["a2", "@reviewer"]],
    runtime: "none (runs the argv directly)",
    driver: "",
    permissionTier: "worktree only",
    executor: "Seatbelt",
    egress: [],
    presenceRequired: false,
    humanGate: false,
    legacyCommand: false,
  };

  it("shows the exact argv, the sandbox, no network, and the producing agent", () => {
    render(<PlanBody plan={plan} target={null} setTarget={() => {}} />);
    expect(screen.getByTestId("plan-argv")).toHaveTextContent('cargo test -- "it\'s"');
    expect(screen.getByText("Seatbelt")).toBeInTheDocument();
    expect(screen.getByText("none")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Target agent" })).toHaveValue("a1");
  });

  it("shows invisible and direction-changing characters in an argv instead of rendering them", () => {
    // U+202E reverses the text after it; U+200B is invisible. Either would let
    // the preview read differently from what runs.
    render(<PlanBody plan={{ ...plan, argv: ["rm", "-rf", "safe\u202Etxt.sh", "a\u200Bb", "x\ny"] }} target={null} setTarget={() => {}} />);
    const shown = screen.getByTestId("plan-argv").textContent ?? "";
    expect(shown).not.toMatch(/[\u200B-\u200F\u202A-\u202E\u2066-\u2069\n]/);
    expect(shown).toContain("\\u202e");
    expect(shown).toContain("\\u200b");
  });

  it("shows the exact instruction text an agent gets, and lets you pick another agent", () => {
    const setTarget = vi.fn();
    const instruction = "An approved proposal (decision) is ready to implement. Carry it out now using your tools, then briefly confirm what you did.\n\n## Ship\n\nbody";
    render(
      <PlanBody
        plan={{ ...plan, kind: "instruction", argv: undefined, instruction, driver: "acp", runtime: "copilot (Copilot)", egress: ["api.githubcopilot.com:443"] }}
        target={null}
        setTarget={setTarget}
      />,
    );
    expect(screen.getByTestId("plan-instruction").textContent).toBe(instruction);
    expect(screen.getByText(/ACP/)).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Target agent" }), { target: { value: "a2" } });
    expect(setTarget).toHaveBeenCalledWith("a2");
  });
});

describe("agent asks", () => {
  it("show open questions with their options, and answer one", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <AgentAsks sessionId="s1" />
      </QueryClientProvider>,
    );
    const card = await screen.findByTestId("ask-card");
    expect(within(card).getByText("\u201CWhich database?\u201D")).toBeInTheDocument();
    expect(screen.queryByText("Old one")).toBeNull();
    fireEvent.click(within(card).getByRole("button", { name: "postgres" }));
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("answer_ask", { sessionId: "s1", askId: "q1", optionIndex: 1, text: null, decline: null }));
  });
});
