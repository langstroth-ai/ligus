import { describe, expect, it } from "vitest";
import { limitedLabel } from "./syncState";

describe("limitedLabel", () => {
  it("names each relay refusal the backend reports", () => {
    expect(limitedLabel("workspace full: the relay is not taking new writes (room_bytes)")).toBe("Workspace full");
    expect(limitedLabel("organisation quota reached on the relay (tenant_bytes): ask your admin")).toBe(
      "Organisation quota reached",
    );
    expect(limitedLabel("room expired or truncated: the relay's history starts at envelope 41")).toBe(
      "Room expired or truncated",
    );
    expect(limitedLabel("the relay is rate limiting this device (write_rate); retrying in 2 min")).toBe(
      "Sync slowed by the relay",
    );
    expect(limitedLabel("the relay is full (store_bytes); new events wait")).toBe("Relay full");
    expect(limitedLabel("something new")).toBe("Sync limited");
    expect(limitedLabel(null)).toBe("Sync limited");
  });
});
