import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseAst } from "rolldown/parseAst";
import { Kbd, TooltipLayer, tip } from "./Tooltip";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function setup() {
  return render(
    <>
      <TooltipLayer />
      <button {...tip("Close panel", "Esc")} aria-label="Close">
        x
      </button>
      <button data-tip="Save">Save</button>
      <span data-tip="Other">o</span>
    </>,
  );
}

describe("tooltips", () => {
  it("show after a pause on hover, with the shortcut, and describe the element", () => {
    setup();
    const btn = screen.getByRole("button", { name: "Close" });
    fireEvent.pointerOver(btn);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
    act(() => vi.advanceTimersByTime(500));
    const t = screen.getByRole("tooltip");
    expect(t).toHaveTextContent("Close panel");
    expect(t.querySelector("kbd")).toHaveTextContent("Esc");
    expect(btn).toHaveAttribute("aria-describedby", t.id);
    fireEvent.pointerOut(btn);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
    expect(btn).not.toHaveAttribute("aria-describedby");
  });

  it("move between neighbours without waiting again", () => {
    setup();
    fireEvent.pointerOver(screen.getByRole("button", { name: "Close" }));
    act(() => vi.advanceTimersByTime(500));
    fireEvent.pointerOut(screen.getByRole("button", { name: "Close" }));
    fireEvent.pointerOver(screen.getByText("o"));
    expect(screen.getByRole("tooltip")).toHaveTextContent("Other");
  });

  it("don't describe an element with its own name, and Esc hides them", () => {
    setup();
    const save = screen.getByRole("button", { name: "Save" });
    fireEvent.pointerOver(save);
    act(() => vi.advanceTimersByTime(500));
    expect(screen.getByRole("tooltip")).toHaveTextContent("Save");
    expect(save).not.toHaveAttribute("aria-describedby");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("cancel a pending show when the pointer leaves or clicks", () => {
    setup();
    const btn = screen.getByRole("button", { name: "Close" });
    fireEvent.pointerOver(btn);
    fireEvent.pointerDown(btn);
    act(() => vi.advanceTimersByTime(800));
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("tip() gives nothing for an empty text", () => {
    expect(tip("")).toEqual({});
    expect(tip("Copy")).toEqual({ "data-tip": "Copy" });
  });

  it("Kbd renders a keyboard chip", () => {
    render(<Kbd>⌘K</Kbd>);
    expect(screen.getByText("⌘K").tagName).toBe("KBD");
  });
});

// The browser's own title tooltip ignores focus and the theme; intrinsic
// elements use data-tip instead. (Components may still take a `title` prop.)
// Parsed with Vite's own parser (rolldown) so nested JSX can't confuse it.
const sources = import.meta.glob(["./**/*.tsx", "../App.tsx"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const PENDING = ["/Sidebar.tsx", "/settings/ModelsSection.tsx"];
// title names an iframe, is a real element inside SVG, and is the only hint a
// native <option> can carry.
const ALLOWED_TAGS = new Set(["iframe", "svg", "title", "option"]);

type Node = { type?: string; [k: string]: unknown };
function walk(n: unknown, fn: (n: Node) => void) {
  if (!n || typeof n !== "object") return;
  if (Array.isArray(n)) return n.forEach((c) => walk(c, fn));
  const node = n as Node;
  if (typeof node.type === "string") fn(node);
  for (const k of Object.keys(node)) walk(node[k], fn);
}

describe("no native title tooltips", () => {
  it("intrinsic elements use data-tip, not title", () => {
    const offenders: string[] = [];
    for (const [path, src] of Object.entries(sources)) {
      if (path.includes(".test.") || PENDING.some((p) => path.endsWith(p))) continue;
      const ast = parseAst(src, { lang: "tsx" });
      // The composer's route pill is being changed elsewhere.
      const skip: [number, number][] = [];
      walk(ast, (n) => {
        if (n.type !== "JSXElement") return;
        const attrs = (n.openingElement as { attributes: { name?: { name: string }; value?: { value?: unknown } }[] }).attributes;
        const cls = attrs.find((a) => a.name?.name === "className")?.value?.value;
        if (typeof cls === "string" && /\brp-wrap\b/.test(cls)) skip.push([n.start as number, n.end as number]);
      });
      walk(ast, (n) => {
        if (n.type !== "JSXOpeningElement") return;
        const name = n.name as { type: string; name?: string };
        if (name.type !== "JSXIdentifier" || !/^[a-z]/.test(name.name ?? "") || ALLOWED_TAGS.has(name.name!)) return;
        for (const a of n.attributes as { type: string; name?: { name: string }; start: number }[]) {
          if (a.type !== "JSXAttribute" || a.name?.name !== "title") continue;
          if (skip.some(([from, to]) => a.start >= from && a.start <= to)) continue;
          offenders.push(`${path}:${src.slice(0, a.start).split("\n").length} <${name.name}>`);
        }
      });
    }
    expect(offenders).toEqual([]);
  });
});
