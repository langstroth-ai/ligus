import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DecisionCard, HOLD_MS, settledLine, waitingOn } from "./DecisionCard";
import type { AskDto } from "@/lib/ipc";

vi.mock("@/lib/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  answerAsk: vi.fn(async () => {}),
}));

function ask(over: Partial<AskDto> = {}): AskDto {
  return {
    askId: "a1",
    chatId: "c1",
    turnId: "t1",
    agentName: "Builder",
    kind: "question",
    v: 2,
    question: "Use Postgres or SQLite for the test fixtures? See https://evil.example/login",
    options: ["Postgres", "SQLite"],
    allowText: true,
    audience: { kind: "people", memberIds: ["me"], names: ["Mike"] },
    quorum: 1,
    eligible: true,
    effect: null,
    origin: null,
    tally: [0, 0],
    expiresAtMs: Date.now() + 60_000,
    open: true,
    live: true,
    declined: false,
    answer: null,
    answerText: null,
    answeredBy: [],
    olderAnswerUncounted: false,
    postedBy: null,
    atMs: Date.now(),
    ...over,
  };
}

function mount(a: AskDto) {
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <DecisionCard sessionId="c1" ask={a} />
    </QueryClientProvider>,
  );
}

const grant = (over: Partial<NonNullable<AskDto["effect"]>> = {}): Partial<AskDto> => ({
  kind: "permission",
  question: "Builder asks permission to run this once.",
  options: ["Allow once", "Deny"],
  allowText: false,
  effect: {
    kind: "permission",
    tool: "execute",
    title: "Publish dry run",
    detail: "cargo publish --dry-run",
    cwd: "/work/shop",
    hostDeviceId: "d1",
    hostLabel: "Mike's mbp",
    presenceRequired: false,
    standing: false,
    tooLarge: false,
    ...over,
  },
});

describe("DecisionCard", () => {
  beforeEach(async () => {
    const ipc = await import("@/lib/ipc");
    vi.mocked(ipc.answerAsk).mockClear();
    try {
      localStorage.clear();
    } catch {
      // no storage in this environment
    }
  });
  afterEach(() => vi.useRealTimers());

  it("shows the agent's words as plain text, never a link", () => {
    const { container } = mount(ask({ question: "Click [here](https://evil.example) or https://evil.example/login **now**" }));
    expect(container.querySelector("a")).toBeNull();
    const quote = container.querySelector('[data-part="quote"]')!;
    expect(quote.textContent).toContain("[here](https://evil.example)");
    expect(quote.textContent).toContain("**now**");
    expect(quote.querySelector("strong")).toBeNull();
  });

  it("keeps the effect line outside the quote, in the order header, quote, options, effect", () => {
    const { container } = mount(ask());
    const parts = [...container.querySelectorAll("[data-part]")].map((e) => e.getAttribute("data-part"));
    expect(parts).toEqual(["header", "quote", "options", "effect"]);
    const quote = container.querySelector('[data-part="quote"]')!;
    const effect = container.querySelector('[data-part="effect"]')!;
    expect(quote.contains(effect)).toBe(false);
    expect(effect.textContent).toBe("Builder decides what to do next. Your answer grants no new permission.");
    expect(screen.getByText(/asks you/)).toBeInTheDocument();
  });

  it("answers with an option, a reply, or Not now", async () => {
    const ipc = await import("@/lib/ipc");
    mount(ask());
    await userEvent.click(screen.getByRole("button", { name: "SQLite" }));
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", { optionIndex: 1 });
    await userEvent.click(screen.getByRole("button", { name: "Reply…" }));
    await userEvent.type(screen.getByRole("textbox", { name: "Your reply" }), "keep the dev db{Enter}");
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", { text: "keep the dev db" });
    await userEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", { decline: true });
  });

  it("picks with keys 1 to 5 while the card has focus, and Enter confirms", async () => {
    const ipc = await import("@/lib/ipc");
    mount(ask({ options: ["a", "b", "c"] }));
    const card = screen.getByTestId("ask-card");
    card.focus();
    fireEvent.keyDown(card, { key: "3" });
    expect(screen.getByRole("button", { name: "c" })).toHaveAttribute("aria-pressed", "true");
    expect(ipc.answerAsk).not.toHaveBeenCalled();
    fireEvent.keyDown(card, { key: "Enter" });
    await act(async () => {});
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", { optionIndex: 2 });
  });

  it("on an older card, Not now only hides it here", async () => {
    const ipc = await import("@/lib/ipc");
    mount(ask({ v: 0, audience: { kind: "room", minRole: "contributor" }, allowText: false }));
    await userEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(ipc.answerAsk).not.toHaveBeenCalled();
    expect(screen.queryByTestId("ask-card")).toBeNull();
  });

  it("a grant needs the 2-second hold; keys don't grant", async () => {
    vi.useFakeTimers();
    const ipc = await import("@/lib/ipc");
    const { container } = mount(ask(grant()));
    expect(container.querySelector('[data-part="effect"]')!.textContent).toContain("Allow once lets Builder run this once on Mike's mbp:");
    expect(screen.getByTestId("ask-detail").textContent).toBe("cargo publish --dry-run");
    expect(screen.getByText("Builder describes it as: Publish dry run")).toBeInTheDocument();
    const card = screen.getByTestId("ask-card");
    fireEvent.keyDown(card, { key: "1" });
    fireEvent.keyDown(card, { key: "Enter" });
    const allow = screen.getByRole("button", { name: /Allow once \(hold/ });
    fireEvent.pointerDown(allow);
    act(() => vi.advanceTimersByTime(HOLD_MS - 100));
    fireEvent.pointerUp(allow);
    await act(async () => {});
    expect(ipc.answerAsk).not.toHaveBeenCalled();
    fireEvent.pointerDown(allow);
    act(() => vi.advanceTimersByTime(HOLD_MS));
    await act(async () => {});
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", { optionIndex: 0 });
  });

  it("a grant that needs a passkey is one click (the passkey is the gesture)", async () => {
    const ipc = await import("@/lib/ipc");
    mount(ask(grant({ presenceRequired: true })));
    expect(screen.getByText("Allowing asks for your passkey.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    expect(ipc.answerAsk).toHaveBeenCalledWith("c1", "a1", { optionIndex: 0 });
  });

  it("a call too large to review offers only Deny", () => {
    mount(ask(grant({ tooLarge: true })));
    expect(screen.queryByRole("button", { name: /Allow/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Deny" })).toBeInTheDocument();
    expect(screen.getByText("Too large to review here. Answer at the terminal.")).toBeInTheDocument();
  });

  it("shows others read-only, and settled and expired cards as one line", () => {
    const room = ask({ eligible: false, audience: { kind: "room", minRole: "admin" }, quorum: 2, tally: [1, 0] });
    expect(waitingOn(room)).toBe("Waiting on Admins · 1 of 2");
    mount(room);
    expect(screen.queryByRole("button", { name: "Postgres" })).toBeNull();
    expect(screen.getByText("Waiting on Admins · 1 of 2")).toBeInTheDocument();
    expect(screen.getByText(/needs 2 of you/)).toBeInTheDocument();
    const done = ask({ live: false, open: false, answer: "Postgres", answeredBy: [{ memberId: "a", name: "Ana", atMs: 0, deviceId: "d1" }] });
    expect(settledLine(done)).toBe("Ana answered Postgres");
    expect(settledLine(ask({ live: false, open: true }))).toBe("Builder stopped waiting.");
  });

  it("tags where a card came from", () => {
    mount(ask({ origin: { via: "outside", label: "shop", runtime: "claude-code" }, agentName: "Claude Code" }));
    expect(screen.getByText("outside Ligus")).toBeInTheDocument();
    expect(screen.getByText("Claude Code (outside Ligus)")).toBeInTheDocument();
  });
});
