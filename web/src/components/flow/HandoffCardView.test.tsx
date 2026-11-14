import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { HandoffCardView } from "./HandoffCardView";
import { handoffsByMessage, qualityLine, type HandoffCard } from "@/lib/flow";

const card: HandoffCard = {
  id: "c1",
  from: "a",
  to: "b",
  fromName: "coder",
  toName: "reviewer",
  messageId: "m1",
  task: "@reviewer check the parser edge cases",
  taskClass: "review",
  branch: "ligus/chat/1",
  files: ["src/parser.rs"],
  quality: { testsPassed: true, proposalStatus: "open", turns: 2, costUsd: 0, error: false },
  via: "mention",
  parcel: { keyFiles: ["src/parser.rs"], decisions: ["kept the old API"], ruledOut: ["a regex rewrite"] },
  atMs: 0,
};

describe("hand-off cards", () => {
  it("shows what, where and how good, and opens the parcel", () => {
    render(<HandoffCardView card={card} />);
    expect(screen.getByTestId("handoff-card")).toHaveTextContent("Hand-off@coder@reviewer");
    expect(screen.getByRole("button", { name: "Go to reply" })).toBeInTheDocument();
    expect(screen.getByText(/1 file · tests passed · proposal open · 2 turns so far/)).toBeInTheDocument();
    expect(screen.queryByText("a regex rewrite")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Context parcel"));
    expect(screen.getByText("a regex rewrite")).toBeInTheDocument();
  });

  it("jumps to the receiving agent's reply, or says it hasn't come yet", () => {
    render(
      <>
        <div className="tt" data-message-id="m1" data-author="coder" />
        <div className="tt" data-message-id="m2" data-author="reviewer" />
        <HandoffCardView card={card} />
      </>,
    );
    const reply = document.querySelector<HTMLElement>('[data-message-id="m2"]')!;
    reply.scrollIntoView = () => {};
    window.matchMedia = ((q: string) => ({ matches: false, media: q })) as unknown as typeof window.matchMedia;
    fireEvent.click(screen.getByRole("button", { name: "Go to reply" }));
    expect(reply.classList.contains("hive-flash")).toBe(true);
    reply.dataset.author = "someone-else";
    fireEvent.click(screen.getByRole("button", { name: "Go to reply" }));
    expect(screen.getByText("@reviewer hasn't replied yet.")).toBeInTheDocument();
  });

  it("hides the parcel toggle when there is none", () => {
    render(<HandoffCardView card={{ ...card, parcel: undefined }} />);
    expect(screen.queryByText("Context parcel")).not.toBeInTheDocument();
  });

  it("indexes cards by message and summarises quality", () => {
    const m = handoffsByMessage([card, { ...card, id: "c2", messageId: undefined }]);
    expect(m.get("m1")?.length).toBe(1);
    expect(qualityLine({ turns: 0, costUsd: 0, error: true, testsPassed: false })).toBe("tests failing · previous turn errored");
  });
});
