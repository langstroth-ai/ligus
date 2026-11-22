import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  AvatarStack,
  Combobox,
  Countdown,
  DataTable,
  Fingerprint,
  InboxItem,
  PresenceDot,
  SessionCard,
  SessionStatus,
  Sheet,
  StatusBar,
  StatusBarItem,
  Stepper,
  VirtualList,
  formatRemaining,
  groupFingerprint,
  presenceColor,
  spellFingerprint,
  visibleRange,
} from "./index";

afterEach(() => vi.useRealTimers());

describe("SessionStatus", () => {
  it("shows the state's glyph and word, plus elapsed time", () => {
    render(<SessionStatus state="working" elapsed="0:42" />);
    expect(screen.getByText("Working")).toBeInTheDocument();
    expect(screen.getByText("· 0:42")).toBeInTheDocument();
  });

  it("renders nothing when idle", () => {
    const { container } = render(<SessionStatus state="idle" />);
    expect(container).toBeEmptyDOMElement();
  });

  it("announces a change of state, not the first render", () => {
    const { rerender } = render(<SessionStatus state="working" announce label="Fix flaky test" />);
    expect(screen.getByRole("status")).toHaveTextContent("");
    rerender(<SessionStatus state="done" announce label="Fix flaky test" />);
    expect(screen.getByRole("status")).toHaveTextContent("Fix flaky test: Done");
  });
});

describe("SessionCard", () => {
  it("opens on click and shows its state in place of the time", () => {
    const onOpen = vi.fn();
    render(<SessionCard title="Fix flaky test" state="needsYou" when="3m" meta="Runs on MacBook Pro" onOpen={onOpen} />);
    expect(screen.getByText("Needs you")).toBeInTheDocument();
    expect(screen.queryByText("3m")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Fix flaky test/ }));
    expect(onOpen).toHaveBeenCalledOnce();
  });

  it("pulses once when the state changes", () => {
    vi.useFakeTimers();
    const { container, rerender } = render(<SessionCard title="A" state="working" />);
    const card = container.querySelector("[data-session-card]")!;
    expect(card.className).not.toContain("ligus-state-pulse");
    rerender(<SessionCard title="A" state="done" />);
    expect(card.className).toContain("ligus-state-pulse");
    act(() => vi.advanceTimersByTime(400));
    expect(card.className).not.toContain("ligus-state-pulse");
  });
});

describe("Sheet", () => {
  it("is a labelled dialog with steps; Esc and the close button close it", () => {
    const onClose = vi.fn();
    render(
      <Sheet title="Add a phone" steps={["Name", "Scan", "Confirm"]} step={1} onClose={onClose} footer={<button>Next</button>}>
        body
      </Sheet>,
    );
    const dialog = screen.getByRole("dialog", { name: "Add a phone" });
    expect(within(dialog).getByText("Scan").closest("li")).toHaveAttribute("aria-current", "step");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe("Stepper", () => {
  it("marks finished steps in words, not only colour", () => {
    render(<Stepper steps={["Name", "Scan", "Confirm"]} current={2} />);
    expect(screen.getByText("Name")).toHaveTextContent("Name (done)");
    expect(screen.getByText("Confirm").closest("li")).toHaveAttribute("aria-current", "step");
  });
});

describe("DataTable", () => {
  const rows = [
    { id: "a", name: "builder", runs: 3 },
    { id: "b", name: "Ada", runs: 12 },
    { id: "c", name: "scout", runs: 1 },
  ];
  const columns = [
    { key: "name", header: "Name", render: (r: (typeof rows)[0]) => r.name, sortValue: (r: (typeof rows)[0]) => r.name },
    { key: "runs", header: "Runs", render: (r: (typeof rows)[0]) => r.runs, sortValue: (r: (typeof rows)[0]) => r.runs, align: "right" as const },
  ];
  const names = () => screen.getAllByRole("row").slice(1).map((r) => r.firstChild!.textContent);

  it("sorts by a column, ascending then descending then off", () => {
    render(<DataTable columns={columns} rows={rows} rowKey={(r) => r.id} />);
    expect(names()).toEqual(["builder", "Ada", "scout"]);
    const runs = screen.getByRole("button", { name: /Runs/ });
    fireEvent.click(runs);
    expect(names()).toEqual(["scout", "builder", "Ada"]);
    expect(screen.getByRole("columnheader", { name: /Runs/ })).toHaveAttribute("aria-sort", "ascending");
    fireEvent.click(runs);
    expect(names()).toEqual(["Ada", "builder", "scout"]);
    fireEvent.click(runs);
    expect(names()).toEqual(["builder", "Ada", "scout"]);
    fireEvent.click(screen.getByRole("button", { name: /Name/ }));
    expect(names()).toEqual(["Ada", "builder", "scout"]);
  });

  it("opens a row on click or Enter and shows an empty message", () => {
    const onRowClick = vi.fn();
    const { rerender } = render(<DataTable columns={columns} rows={rows} rowKey={(r) => r.id} onRowClick={onRowClick} />);
    fireEvent.keyDown(screen.getAllByRole("row")[2], { key: "Enter" });
    expect(onRowClick).toHaveBeenCalledWith(rows[1]);
    rerender(<DataTable columns={columns} rows={[]} rowKey={(r) => r.id} empty="No agents yet" />);
    expect(screen.getByText("No agents yet")).toBeInTheDocument();
  });
});

describe("Combobox", () => {
  const options = [
    { value: "mbp", label: "MacBook Pro", hint: "This device" },
    { value: "box", label: "Serve box", hint: "Linux" },
    { value: "ci", label: "CI worker", disabled: true, disabledReason: "Offline" },
  ];
  function Harness() {
    const [v, setV] = useState<string | null>("mbp");
    return <Combobox label="Host" options={options} value={v} onChange={setV} />;
  }

  it("filters as you type and picks with Enter", () => {
    render(<Harness />);
    const input = screen.getByRole("combobox", { name: "Host" });
    expect(input).toHaveValue("MacBook Pro");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "serve" } });
    expect(screen.getAllByRole("option")).toHaveLength(1);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(input).toHaveValue("Serve box");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("skips disabled options and shows why", () => {
    render(<Harness />);
    const input = screen.getByRole("combobox");
    fireEvent.focus(input);
    expect(screen.getByText("Offline")).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "ArrowDown" }); // box
    fireEvent.keyDown(input, { key: "ArrowDown" }); // skips ci, wraps to mbp
    expect(input.getAttribute("aria-activedescendant")).toMatch(/opt-mbp$/);
    fireEvent.click(screen.getByText("CI worker"));
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(input).toHaveValue("MacBook Pro");
  });
});

describe("Fingerprint", () => {
  it("groups by four and spells it for screen readers", () => {
    expect(groupFingerprint("ab12-cd34 ef5")).toEqual(["AB12", "CD34", "EF5"]);
    expect(spellFingerprint(["AB12", "C"])).toBe("A B 1 2, C");
    render(<Fingerprint value="ab12cd34" label="Device fingerprint" />);
    expect(screen.getByRole("img", { name: "Device fingerprint: A B 1 2, C D 3 4" })).toBeInTheDocument();
  });

  it("copies the grouped value", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<Fingerprint value="ab12cd34" />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy fingerprint" }));
    });
    expect(writeText).toHaveBeenCalledWith("AB12 CD34");
    expect(screen.getByRole("button", { name: "Fingerprint copied" })).toBeInTheDocument();
  });
});

describe("Presence", () => {
  it("carries presence in shape and words, not only colour", () => {
    render(
      <>
        <PresenceDot presence="active" name="Maya" />
        <PresenceDot presence="offline" name="Zaz" />
      </>,
    );
    expect(screen.getByRole("img", { name: "Maya: Active now" })).toHaveAttribute("data-presence", "active");
    const off = screen.getByRole("img", { name: "Zaz: Offline" });
    expect(off).toHaveAttribute("data-presence", "offline");
    expect(off.style.border).toContain("dashed");
  });

  it("stacks avatars with +N and names everyone", () => {
    const people = ["Maya", "Zaz", "Ada", "Lin", "Bo"].map((name) => ({ name }));
    render(<AvatarStack people={people} max={3} />);
    expect(screen.getByRole("img", { name: "Maya, Zaz, Ada, Lin, Bo" })).toBeInTheDocument();
    expect(screen.getByText("+2")).toBeInTheDocument();
  });

  it("cycles through eight presence colours", () => {
    expect(presenceColor(1)).toContain("--ligus-presence-1");
    expect(presenceColor(9)).toContain("--ligus-presence-1");
    expect(presenceColor(8)).toContain("--ligus-presence-8");
  });
});

describe("InboxItem", () => {
  it("opens from the row; its actions don't open it", () => {
    const onOpen = vi.fn();
    const onAnswer = vi.fn();
    render(
      <div role="list">
        <InboxItem title="Vote: cold-day floor" context="#general" when="2m" unread onOpen={onOpen} actions={<button onClick={onAnswer}>Answer</button>} />
      </div>,
    );
    expect(screen.getByText("Needs you")).toBeInTheDocument();
    expect(screen.getByText("(new)")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    expect(onAnswer).toHaveBeenCalled();
    expect(onOpen).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /Vote: cold-day floor/ }));
    expect(onOpen).toHaveBeenCalled();
  });
});

describe("StatusBar", () => {
  it("lays out items and hides without unmounting", () => {
    const onClick = vi.fn();
    const { rerender } = render(
      <StatusBar start={<StatusBarItem live>Synced</StatusBarItem>} end={<StatusBarItem onClick={onClick}>Update ready</StatusBarItem>} />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Synced");
    fireEvent.click(screen.getByRole("button", { name: "Update ready" }));
    expect(onClick).toHaveBeenCalled();
    rerender(<StatusBar hidden start={<StatusBarItem>Synced</StatusBarItem>} />);
    expect(screen.getByText("Synced").closest("footer")).toHaveAttribute("hidden");
  });
});

describe("Countdown", () => {
  it("formats the remaining time", () => {
    expect(formatRemaining(245_000)).toBe("4:05");
    expect(formatRemaining(8_200)).toBe("0:09");
    expect(formatRemaining(3_720_000)).toBe("1:02:00");
    expect(formatRemaining(-5)).toBe("0:00");
  });

  it("ticks, announces only near the end, and calls onExpire once", () => {
    vi.useFakeTimers();
    let t = 0;
    const now = () => t;
    const onExpire = vi.fn();
    render(<Countdown expiresAt={30_000} totalMs={60_000} now={now} onExpire={onExpire} />);
    const timer = screen.getByRole("timer");
    expect(timer).toHaveTextContent("0:30");
    expect(timer).toHaveAttribute("aria-live", "off");
    t = 21_000;
    act(() => vi.advanceTimersByTime(1000));
    expect(timer).toHaveTextContent("0:09");
    expect(timer).toHaveAttribute("aria-live", "polite");
    t = 31_000;
    act(() => vi.advanceTimersByTime(1000));
    act(() => vi.advanceTimersByTime(1000));
    expect(timer).toHaveTextContent("Expired");
    expect(onExpire).toHaveBeenCalledTimes(1);
  });
});

describe("VirtualList", () => {
  it("computes the visible window with overscan", () => {
    const offsets = [0, 50, 100, 150, 200, 250, 300];
    expect(visibleRange(offsets, 0, 100, 0)).toEqual([0, 1]);
    expect(visibleRange(offsets, 120, 100, 1)).toEqual([1, 5]);
    expect(visibleRange([0], 0, 100, 2)).toEqual([0, -1]);
  });

  it("renders only rows near the viewport", () => {
    const items = Array.from({ length: 1000 }, (_, i) => `row ${i}`);
    render(<VirtualList items={items} itemKey={(s) => s} renderItem={(s) => <div>{s}</div>} estimateHeight={40} role="log" ariaLabel="Transcript" />);
    const log = screen.getByRole("log", { name: "Transcript" });
    expect(log).toHaveAttribute("aria-relevant", "additions");
    expect(screen.getByText("row 0")).toBeInTheDocument();
    expect(screen.queryByText("row 500")).not.toBeInTheDocument();
    expect(log.querySelectorAll("[data-index]").length).toBeLessThan(40);
  });
});
