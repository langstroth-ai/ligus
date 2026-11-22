import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { ToastHost, toast, errMsg, errorLogStore, _resetToasts } from "./Toast";
import { ErrorLogSheet } from "./ErrorLogSheet";

describe("errMsg", () => {
  it("unwraps Error, string, and objects", () => {
    expect(errMsg(new Error("boom"))).toBe("boom");
    expect(errMsg("plain")).toBe("plain");
    expect(errMsg({ code: 42 })).toBe('{"code":42}');
  });
});

describe("toast store + host", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    _resetToasts();
  });
  afterEach(() => {
    // Flush any pending auto-dismiss timers so the module store is clean.
    act(() => vi.runAllTimers());
    vi.useRealTimers();
  });

  it("renders a dispatched toast and auto-dismisses after its TTL", () => {
    render(<ToastHost />);
    act(() => {
      toast.success("Saved");
    });
    expect(screen.getByText("Saved")).toBeInTheDocument();
    // success TTL is 3s.
    act(() => vi.advanceTimersByTime(3000));
    expect(screen.queryByText("Saved")).not.toBeInTheDocument();
  });

  it("errors linger longer than successes", () => {
    render(<ToastHost />);
    act(() => {
      toast.error("Nope");
    });
    act(() => vi.advanceTimersByTime(3000));
    // Still visible at 3s (error TTL is 7s).
    expect(screen.getByText("Nope")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(4000));
    expect(screen.queryByText("Nope")).not.toBeInTheDocument();
  });

  it("invokes a toast action and then dismisses it", () => {
    render(<ToastHost />);
    const run = vi.fn();
    act(() => {
      toast.error("Failed", { label: "Retry", run });
    });
    // fireEvent is synchronous — avoids userEvent's delay loop under fake timers.
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(run).toHaveBeenCalledOnce();
    expect(screen.queryByText("Failed")).not.toBeInTheDocument();
  });

  it("shows one toast at a time; the next one waits its turn", () => {
    render(<ToastHost />);
    act(() => {
      toast.success("First");
      toast.info("Second");
    });
    expect(screen.getByText("First")).toBeInTheDocument();
    expect(screen.queryByText("Second")).not.toBeInTheDocument();
    expect(screen.getByText(/1 more/)).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(3000));
    expect(screen.queryByText("First")).not.toBeInTheDocument();
    expect(screen.getByText("Second")).toBeInTheDocument();
    // The waiting toast's clock starts when it shows, not when it was queued.
    act(() => vi.advanceTimersByTime(3500));
    expect(screen.getByText("Second")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(500));
    expect(screen.queryByText("Second")).not.toBeInTheDocument();
  });

  it("a burst of the same message counts once, and only a few wait", () => {
    render(<ToastHost />);
    act(() => {
      for (let i = 0; i < 4; i++) toast.error("Relay unreachable");
      for (let i = 0; i < 6; i++) toast.info(`note ${i}`);
    });
    expect(screen.getAllByText("Relay unreachable")).toHaveLength(1);
    expect(screen.getByText(/3 more/)).toBeInTheDocument();
    expect(errorLogStore.snapshot().entries.map((e) => e.message)).toEqual(["Relay unreachable"]);
  });

  it("keeps every error in a log you can reopen after the toast is gone", () => {
    render(<ToastHost />);
    act(() => {
      toast.error("Couldn't send your answer: offline");
      toast.success("Saved");
    });
    act(() => vi.runAllTimers());
    expect(screen.queryByText(/Couldn't send/)).not.toBeInTheDocument();
    expect(errorLogStore.snapshot().unseen).toBe(1);
    const onClose = vi.fn();
    render(<ErrorLogSheet onClose={onClose} />);
    expect(screen.getByText("Couldn't send your answer: offline")).toBeInTheDocument();
    expect(errorLogStore.snapshot().unseen).toBe(0);
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    });
    expect(screen.getByText(/No errors since Ligus started/)).toBeInTheDocument();
  });
});
