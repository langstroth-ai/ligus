import { afterEach, describe, expect, it } from "vitest";
import { applyDensity, loadDensity, saveDensity } from "./density";

afterEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset.density;
});

describe("density", () => {
  it("defaults to comfortable", () => {
    expect(loadDensity()).toBe("comfortable");
    localStorage.setItem("ligus.density", "huge");
    expect(loadDensity()).toBe("comfortable");
  });

  it("round-trips and applies as data-density on <html>", () => {
    saveDensity("compact");
    expect(loadDensity()).toBe("compact");
    applyDensity("spacious");
    expect(document.documentElement.dataset.density).toBe("spacious");
  });
});
