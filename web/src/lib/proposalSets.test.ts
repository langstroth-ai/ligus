import { describe, expect, it } from "vitest";
import type { ProposalDto } from "@/bindings/ProposalDto";
import { alt, set } from "@/test/proposalSetFixtures";
import { nextStep, phaseLine, supportText, withoutSetAlternatives } from "./proposalSets";

const passed = {
  passed: true,
  tests: { status: "passed", note: "" },
  baseUnchanged: { status: "passed", note: "" },
  hostsOnline: { status: "skipped", note: "" },
  at: "2026-11-13T10:00:00Z",
};

describe("nextStep", () => {
  it("walks approve → pre-flight → apply, and never offers apply before a passing pre-flight", () => {
    const s = set({ phase: "piping", frontRunner: "a" });
    expect(nextStep(s, alt({ proposalId: "a" }))).toBe("approve");
    expect(nextStep(s, alt({ proposalId: "a", status: "approved" }))).toBe("preflight");
    const failed = { ...passed, passed: false, tests: { status: "failed", note: "" } };
    expect(nextStep(s, alt({ proposalId: "a", status: "approved", preflight: failed }))).toBe("preflight");
    expect(nextStep(s, alt({ proposalId: "a", status: "approved", preflight: passed }))).toBe("apply");
  });

  it("offers nothing on stopped alternatives and revert only on the applied winner", () => {
    const s = set({ phase: "piping" });
    expect(nextStep(s, alt({ proposalId: "b", active: false }))).toBeNull();
    const done = set({ phase: "decided", winner: "a" });
    expect(nextStep(done, alt({ proposalId: "a", status: "applied", kind: "fileDiff" }))).toBe("revert");
    expect(nextStep(done, alt({ proposalId: "a", status: "applied", kind: "decision" }))).toBeNull();
    expect(nextStep(done, alt({ proposalId: "b", status: "approved" }))).toBeNull();
  });
});

describe("withoutSetAlternatives", () => {
  it("keeps set alternatives out of the plain inbox", () => {
    const p = (id: string, setId?: string) => ({ id, setId }) as unknown as ProposalDto;
    const sets = [set({ alternatives: [alt({ proposalId: "a" })] })];
    expect(withoutSetAlternatives([p("a", "s1"), p("x"), p("y", "unknown-set")], sets).map((q) => q.id)).toEqual([
      "x",
      "y",
    ]);
  });
});

describe("phaseLine / supportText", () => {
  it("describes each phase", () => {
    expect(phaseLine(set({}))).toMatch(/2 independent scouts at 70\+/);
    expect(phaseLine(set({ attempt: 1 }))).toMatch(/pre-flight failed/);
    const piping = set({ phase: "piping", frontRunner: "a", tieBreak: true, alternatives: [alt({ proposalId: "a", label: "B" })] });
    expect(phaseLine(piping)).toBe("B is the front-runner (tie-break at the deadline). Rivals are frozen; your call.");
    expect(phaseLine(set({ phase: "decided" }))).toBe("Closed without choosing.");
  });
  it("shows decay", () => {
    expect(supportText(80, 80)).toBe("80");
    expect(supportText(80, 55)).toBe("80 → 55");
  });
});
