import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { leaseSummary, RunLeaseLine } from "./RunLeaseLine";
import type { RunLeaseDto } from "@/bindings/RunLeaseDto";

const base: RunLeaseDto = {
  holder: "0d7c5a8e-0000-0000-0000-000000000000",
  holderLabel: "Studio Mac",
  isMe: false,
  epoch: 2,
  leaseMs: 60000,
  health: "live",
  lastBeatAgeMs: 4000,
  verified: true,
  handoverTo: null,
  fencedClaims: 0,
};

describe("leaseSummary", () => {
  it("names the holder and grades health", () => {
    expect(leaseSummary(base)).toEqual({ text: "Driven by Studio Mac (epoch 2)", tone: "ok" });
    expect(leaseSummary({ ...base, health: "degraded", lastBeatAgeMs: 40000 }).text).toContain("heartbeat 40s ago (lease 60s)");
    expect(leaseSummary({ ...base, health: "lapsed", lastBeatAgeMs: 90000 }).tone).toBe("bad");
    expect(leaseSummary({ ...base, isMe: true }).text).toContain("this device");
    expect(leaseSummary({ ...base, health: "released", handoverTo: "x" }).text).toContain("Handed over");
  });
});

describe("RunLeaseLine", () => {
  it("offers hand over only to the holder and shows fenced claims", async () => {
    const onHandOver = vi.fn();
    const { rerender } = render(<RunLeaseLine lease={{ ...base, fencedClaims: 1 }} onHandOver={onHandOver} />);
    expect(screen.queryByText("Hand over")).toBeNull();
    expect(screen.getByText("1 competing claim fenced")).toBeInTheDocument();
    rerender(<RunLeaseLine lease={{ ...base, isMe: true }} onHandOver={onHandOver} />);
    await userEvent.click(screen.getByText("Hand over"));
    expect(onHandOver).toHaveBeenCalled();
  });

  it("flags an unverified holder", () => {
    render(<RunLeaseLine lease={{ ...base, verified: false }} />);
    expect(screen.getByText("unverified")).toBeInTheDocument();
  });
});
