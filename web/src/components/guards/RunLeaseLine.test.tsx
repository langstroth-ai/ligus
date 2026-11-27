import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { handOverCandidates, leaseNotice, leaseSummary, RunLeaseLine } from "./RunLeaseLine";
import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";
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

const host = (id: string, label: string, kind = "device", online = true, ownerName = "Mara", mine = false): WorkspaceHostDto => ({
  id,
  kind,
  label,
  online,
  lastSeen: "2026-11-22T10:00:00Z",
  modelGateways: [],
  ownerName,
  offers: [],
  readiness: "",
  build: "",
  mine,
});

describe("leaseNotice", () => {
  it("stays quiet while the driver beats and labels every other state", () => {
    expect(leaseNotice(base)).toBeNull();
    expect(leaseNotice({ ...base, health: "degraded", lastBeatAgeMs: 40000 })).toEqual({
      title: "Heartbeat late",
      detail: "Studio Mac last checked in 40s ago. If it misses the 60s lease, another device can take over.",
    });
    expect(leaseNotice({ ...base, isMe: true, health: "lapsed", lastBeatAgeMs: 4000 })).toEqual({
      title: "No heartbeat",
      detail: "This device stopped sending heartbeats 4s ago. Another device can take over.",
    });
    expect(leaseNotice({ ...base, health: "released", handoverTo: "x" })?.title).toBe("Handing over");
  });
});

describe("handOverCandidates", () => {
  it("lists online desktops other than the holder", () => {
    const hosts = [
      host(base.holder, "Studio Mac"),
      host("d2", "Mara's laptop"),
      host("d3", "Old Mac", "device", false),
      host("w1", "serve-box", "worker"),
    ];
    expect(handOverCandidates(base, hosts).map((h) => h.id)).toEqual(["d2"]);
  });
});

describe("RunLeaseLine: the heartbeat notice", () => {
  it("is a labelled notice, not a bare dot", () => {
    render(<RunLeaseLine lease={{ ...base, health: "lapsed", lastBeatAgeMs: 90000 }} />);
    const notice = screen.getByRole("status");
    expect(notice).toHaveTextContent("No heartbeat");
    expect(notice).toHaveTextContent("Studio Mac stopped sending heartbeats 90s ago");
    expect(screen.queryByText("●")).toBeNull();
    // Only the holder can hand a run over.
    expect(screen.queryByLabelText("Device to hand the run to")).toBeNull();
  });

  it("lets the holder pick the device to hand over to", async () => {
    const onHandOver = vi.fn();
    const lease = { ...base, holder: "me", isMe: true, health: "degraded", lastBeatAgeMs: 40000 };
    render(<RunLeaseLine lease={lease} hosts={[host("me", "This Mac"), host("d2", "Mara's laptop")]} onHandOver={onHandOver} />);
    const picker = screen.getByLabelText("Device to hand the run to");
    expect(Array.from((picker as HTMLSelectElement).options).map((o) => o.text)).toEqual(["Any online device", "Mara's laptop (Mara, d2)"]);
    await userEvent.click(screen.getByRole("button", { name: "Hand over" }));
    expect(onHandOver).toHaveBeenLastCalledWith(undefined);
    await userEvent.selectOptions(picker, "d2");
    await userEvent.click(screen.getByRole("button", { name: "Hand over" }));
    expect(onHandOver).toHaveBeenLastCalledWith("d2");
  });

  it("tells apart devices that share a label by owner and id", () => {
    // Labels are chosen by each member, so another member can name theirs
    // after yours; the picker must still say whose each one is.
    const lease = { ...base, holder: "me", isMe: true, health: "degraded", lastBeatAgeMs: 40000 };
    const hosts = [
      host("me", "This Mac"),
      host("aaaaaaaa-1111", "Studio Mac", "device", true, "Zaz", true),
      host("bbbbbbbb-2222", "Studio Mac", "device", true, "Mallory", false),
      host("cccccccc-3333", "", "device", true, "", false),
    ];
    render(<RunLeaseLine lease={lease} hosts={hosts} onHandOver={vi.fn()} />);
    const picker = screen.getByLabelText("Device to hand the run to") as HTMLSelectElement;
    expect(Array.from(picker.options).map((o) => o.text)).toEqual([
      "Any online device",
      "Studio Mac (yours, aaaaaaaa)",
      "Studio Mac (Mallory, bbbbbbbb)",
      "device (unknown owner, cccccccc)",
    ]);
  });

  it("says so when no other device is online", () => {
    const lease = { ...base, holder: "me", isMe: true, health: "lapsed" };
    render(<RunLeaseLine lease={lease} hosts={[host("me", "This Mac")]} onHandOver={vi.fn()} />);
    expect(screen.getByTestId("hand-over-none")).toHaveTextContent("No other device is online");
  });
});
