import { describe, it, expect, vi } from "vitest";
import css from "../styles.css?raw";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { RightRail } from "./RightRail";
import { Button, IconButton, BUTTON_VARIANT_CLASS } from "./ui";
import type { ProposalDto } from "@/lib/ipc";

// The Review card: buttons that read as clickable, Markdown bodies, and the
// viewer's own vote.

const members = [
  { id: "me", actorId: "me", displayName: "Ana", role: "owner", title: "", index: 1, isSelf: true, avatarUrl: null },
  { id: "maya", actorId: "maya", displayName: "Maya", role: "admin", title: "", index: 2, isSelf: false, avatarUrl: null },
];

let proposals: ProposalDto[] = [];

const base: ProposalDto = {
  id: "p1",
  title: "Cold-day floor",
  body: "**This is a sample proposal.**\n\n**Context**\n\n- Every Monday the scheduler books a check.\n\n[notes](javascript:alert(1))",
  authorActorId: "agent",
  kind: "decision",
  status: "open",
  requiredApprovals: 1,
  qualifyingApprovals: 0,
  quorumMet: false,
  approvals: [],
  createdAt: "2026-11-01T12:00:00Z",
  dismissed: false,
  refinements: [
    { id: "r1", actorId: "maya", displayName: "Maya", role: "admin", text: "Use **15°C**, not 12.", requestChanges: false, createdAt: "2026-11-01T12:05:00Z" },
  ],
} as ProposalDto;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: { approved?: boolean }) => {
    if (cmd === "list_members") return members;
    if (cmd === "list_proposals") return proposals;
    if (cmd === "vote_proposal") {
      proposals = [{ ...proposals[0], approvals: [{ actorId: "me", displayName: "Ana", role: "owner", approved: Boolean(args?.approved) }] }];
      return proposals[0];
    }
    if (cmd?.startsWith("list_")) return [];
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

function review() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <RightRail width={400} sessionId="s1" pane="review" activeRuntimeId="" onChangePane={() => {}} onClose={() => {}} />
    </QueryClientProvider>,
  );
}

describe("button feedback", () => {
  it("gives every Button variant the shared hover/active/focus class", () => {
    for (const variant of Object.keys(BUTTON_VARIANT_CLASS) as (keyof typeof BUTTON_VARIANT_CLASS)[]) {
      const { unmount } = render(<Button variant={variant}>Go</Button>);
      const b = screen.getByRole("button", { name: "Go" });
      expect(b.className).toContain("ligus-btn");
      expect(b.className).toContain(BUTTON_VARIANT_CLASS[variant]);
      // Colours come from the class, not inline styles (inline beats :hover).
      expect(b.getAttribute("style") ?? "").not.toMatch(/background|border/);
      unmount();
    }
    render(<IconButton label="Close">x</IconButton>);
    expect(screen.getByRole("button", { name: "Close" }).className).toContain("ligus-press");
  });

  it("styles.css sets the pointer app-wide and defines calm hover, active and focus states", () => {
    expect(css).toMatch(/button:not\(:disabled\),[\s\S]*?cursor: pointer;/);
    expect(css).toMatch(/\[role="button"\]:not\(\[aria-disabled="true"\]\)/);
    expect(css).toMatch(/a\[href\],/);
    expect(css).toMatch(/\[role="menuitem"\]:not\(\[aria-disabled="true"\]\)/);
    expect(css).toMatch(/button:disabled,[\s\S]*?cursor: not-allowed;/);
    expect(css).toMatch(/\.ligus-btn:not\(:disabled\):not\(\[aria-disabled="true"\]\):hover/);
    expect(css).toMatch(/\.ligus-btn:not\(:disabled\):not\(\[aria-disabled="true"\]\):active/);
    expect(css).toMatch(/\.ligus-btn:focus-visible/);
    // Reduced motion still collapses every transition.
    expect(css).toMatch(/prefers-reduced-motion: reduce[\s\S]*?transition-duration: 0\.01ms !important/);
  });
});

describe("the Review card", () => {
  it("renders the proposal body and refinement notes as Markdown, links kept safe", async () => {
    proposals = [base];
    review();
    const body = await screen.findByTestId("proposal-body");
    expect(body.textContent).not.toContain("**");
    expect(body.querySelector("strong")?.textContent).toBe("This is a sample proposal.");
    expect(body.querySelector("li")?.textContent).toBe("Every Monday the scheduler books a check.");
    // A javascript: link renders without an href.
    const link = [...body.querySelectorAll("a")].find((a) => a.textContent === "notes");
    expect(link?.getAttribute("href")).toBeNull();
    const note = screen.getByTestId("refinement-text");
    expect(note.querySelector("strong")?.textContent).toBe("15°C");
  });

  it("shows Approve and Reject before you vote, then your vote and a way to change it", async () => {
    proposals = [base];
    review();
    const approve = await screen.findByRole("button", { name: "Approve" });
    expect(approve.className).toContain("ligus-btn-success");
    expect(screen.getByRole("button", { name: "Reject" }).className).toContain("ligus-btn-danger");
    fireEvent.click(approve);
    await waitFor(() => expect(screen.getByTestId("my-vote").textContent).toContain("You approved"));
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Reject" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Change to reject" }));
    await waitFor(() =>
      expect(vi.mocked(invoke)).toHaveBeenCalledWith("vote_proposal", expect.objectContaining({ approved: false })),
    );
  });

  it("puts Implement first once quorum is met and you have voted", async () => {
    proposals = [
      {
        ...base,
        status: "approved",
        qualifyingApprovals: 1,
        quorumMet: true,
        approvals: [{ actorId: "me", displayName: "Ana", role: "owner", approved: true }],
      },
    ];
    review();
    expect((await screen.findByTestId("my-vote")).textContent).toContain("You approved");
    expect(screen.getByRole("button", { name: "Implement…" }).className).toContain("ligus-btn-primary");
    expect(screen.getByTestId("next-step").textContent).toContain("Quorum met");
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("hides Implement on a rejected proposal that still has quorum, with the reason", async () => {
    proposals = [
      {
        ...base,
        status: "rejected",
        requiredApprovals: 1,
        qualifyingApprovals: 1,
        quorumMet: true,
        approvals: [
          { actorId: "maya", displayName: "Maya", role: "admin", approved: true },
          { actorId: "me", displayName: "Ana", role: "owner", approved: false },
        ],
      },
    ];
    review();
    // Settled, so it sits collapsed under the settled shelf.
    fireEvent.click(await screen.findByRole("button", { name: /1 settled proposal/ }));
    expect((await screen.findByTestId("next-step")).textContent).toMatch(/Rejected, so it can't be implemented/);
    expect(screen.queryByRole("button", { name: "Implement…" })).toBeNull();
  });

  it("keeps voting quiet beside Implement when quorum is met without you", async () => {
    proposals = [
      {
        ...base,
        status: "approved",
        qualifyingApprovals: 1,
        quorumMet: true,
        approvals: [{ actorId: "maya", displayName: "Maya", role: "admin", approved: true }],
      },
    ];
    review();
    expect((await screen.findByRole("button", { name: "Approve" })).className).toContain("ligus-btn-quiet");
    expect(screen.queryByTestId("my-vote")).toBeNull();
  });
});
