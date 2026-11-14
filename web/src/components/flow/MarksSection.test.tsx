import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MarksView } from "./MarksSection";
import type { MarkRow } from "@/lib/flow";

const mark: MarkRow = {
  id: "m1",
  kind: "underConstruction",
  path: "src/parser.rs",
  lines: [10, 40],
  note: "splitting the lexer",
  agent: "a",
  by: "coder",
  sessionId: "s",
  atMs: 0,
  expiresMs: Date.now() + 30 * 60_000,
  chatTitle: "Parser",
};

describe("workspace marks", () => {
  it("lists marks and clears one", () => {
    const cleared: string[] = [];
    render(<MarksView marks={[mark]} onAdd={() => {}} onClear={(id) => cleared.push(id)} />);
    expect(screen.getByText("Under construction")).toBeInTheDocument();
    expect(screen.getByText(/by coder in “Parser” · 30 min left — splitting the lexer/)).toBeInTheDocument();
    fireEvent.click(screen.getByText("Clear"));
    expect(cleared).toEqual(["m1"]);
  });

  it("leaves a mark", () => {
    const added: string[][] = [];
    render(<MarksView marks={[]} onAdd={(k, p, n) => added.push([k, p, n])} onClear={() => {}} />);
    expect(screen.getByText("No marks in this workspace.")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Mark kind"), { target: { value: "lock" } });
    fireEvent.change(screen.getByLabelText("Path"), { target: { value: " docs/a.md " } });
    fireEvent.click(screen.getByText("Leave mark"));
    expect(added).toEqual([["lock", "docs/a.md", ""]]);
  });
});
