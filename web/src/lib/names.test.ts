import { describe, expect, it } from "vitest";
import { cleanDisplayName } from "./names";

describe("cleanDisplayName", () => {
  it("drops bidi, invisible and control characters and collapses line separators", () => {
    expect(cleanDisplayName("‮adi‬")).toBe("adi");
    expect(cleanDisplayName("A​d‍a⁦⁩")).toBe("Ada");
    expect(cleanDisplayName("﻿Ada Lovelace\u0007")).toBe("Ada Lovelace");
    expect(cleanDisplayName("  Ada \t\n Lovelace  ")).toBe("Ada Lovelace");
    expect(cleanDisplayName("tag\u{E0041}s")).toBe("tags");
    expect(cleanDisplayName("Zoë Ångström 李 🐝")).toBe("Zoë Ångström 李 🐝");
  });
});
