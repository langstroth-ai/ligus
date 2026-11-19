import { describe, it, expect } from "vitest";
import vectors from "../../../crates/ligus-proto/tests/vectors/display-names.json";
import { stripBidi } from "./links";

// The shared display-name vectors (ligus_proto::names, the iOS
// NameSanitizer): stripBidi removes every character names.rs hides, so a
// handle shown on the web reads as the box and the phone show it.
const clean = (s: string) => stripBidi(s).replace(/\s+/gu, " ").trim();

describe("stripBidi on the shared display-name vectors", () => {
  for (const c of vectors.cases) {
    it(c.note, () => {
      expect(clean(c.input)).toBe(c.clean);
    });
  }

  it("strips tag and musical-symbol format characters (supplementary planes)", () => {
    expect(stripBidi("\u{E0041}Bob\u{E007F}")).toBe("Bob");
    expect(stripBidi("Al\u{1D173}ice\u{1D17A}")).toBe("Alice");
    expect(stripBidi("x\u{E0100}y\u{E0FFF}")).toBe("xy");
    // Ordinary astral characters stay.
    expect(stripBidi("🐝 \u{1D11E}")).toBe("🐝 \u{1D11E}");
  });
});
