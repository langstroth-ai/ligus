import { describe, expect, it } from "vitest";
import { parseShortCode, relayHost } from "./shortCode";

describe("parseShortCode", () => {
  it("reads a typed code the way the desktop does", () => {
    for (const t of ["K7Q-4MX", "k7q 4mx", " K7Q4MX ", "k7q‑4mx"]) expect(parseShortCode(t)).toBe("K7Q4MX");
    expect(parseShortCode("O1L-I00")).toBe("011100");
  });
  it("refuses what isn't six code symbols", () => {
    for (const t of ["", "K7Q4M", "K7Q4MXX", "K7Q4MU", "ligusaddr1:abc", "K7Q-4M!"]) expect(parseShortCode(t)).toBeNull();
  });
  it("shows a relay by its host", () => {
    expect(relayHost("https://relay.example.org/")).toBe("relay.example.org");
    expect(relayHost("not a url")).toBe("not a url");
  });
});
