import { useState } from "react";
import type { RunLeaseDto } from "@/bindings/RunLeaseDto";
import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";
import { IconAlertTriangle, IconInfo } from "@/lib/icons";
import { fieldStyle, LinkButton } from "@/components/ui";

function holderName(lease: RunLeaseDto): string {
  return lease.isMe ? "this device" : lease.holderLabel || `device ${lease.holder.slice(0, 8)}`;
}

/// One-line summary of a workflow run's coordinator lease: who drives it and
/// whether that device is still beating. Exported for tests.
export function leaseSummary(lease: RunLeaseDto): { text: string; tone: "ok" | "warn" | "bad" } {
  const who = holderName(lease);
  const age = Math.round(lease.lastBeatAgeMs / 1000);
  const leaseS = Math.round(lease.leaseMs / 1000);
  switch (lease.health) {
    case "live":
      return { text: `Driven by ${who} (epoch ${lease.epoch})`, tone: "ok" };
    case "degraded":
      return { text: `Driven by ${who}, heartbeat ${age}s ago (lease ${leaseS}s)`, tone: "warn" };
    case "released":
      return lease.handoverTo
        ? { text: `Handed over by ${who}; waiting for the next device`, tone: "warn" }
        : { text: `Released by ${who}`, tone: "warn" };
    default:
      return { text: `No heartbeat from ${who} for ${age}s: another device can take over`, tone: "bad" };
  }
}

/// The labelled notice for a run whose driver isn't beating normally, or
/// null while it is. Exported for tests.
export function leaseNotice(lease: RunLeaseDto): { title: string; detail: string } | null {
  const who = holderName(lease);
  const Who = who.charAt(0).toUpperCase() + who.slice(1);
  const age = Math.round(lease.lastBeatAgeMs / 1000);
  const leaseS = Math.round(lease.leaseMs / 1000);
  switch (lease.health) {
    case "live":
      return null;
    case "degraded":
      return {
        title: "Heartbeat late",
        detail: `${Who} last checked in ${age}s ago. If it misses the ${leaseS}s lease, another device can take over.`,
      };
    case "released":
      return lease.handoverTo
        ? { title: "Handing over", detail: `${Who} let go of this run; the next device picks it up on its next heartbeat.` }
        : { title: "Released", detail: `${Who} let go of this run.` };
    default:
      return {
        title: "No heartbeat",
        detail: `${Who} stopped sending heartbeats ${age}s ago. Another device can take over.`,
      };
  }
}

/// Devices that could drive the run instead: online desktops of the
/// workspace other than the current holder (workers don't drive workflows).
export function handOverCandidates(lease: RunLeaseDto, hosts: WorkspaceHostDto[]): WorkspaceHostDto[] {
  return hosts.filter((h) => h.kind !== "worker" && h.online && h.id !== lease.holder);
}

export function RunLeaseLine({
  lease,
  hosts = [],
  onHandOver,
}: {
  lease: RunLeaseDto;
  /// The workspace's hosts, for the hand-over picker.
  hosts?: WorkspaceHostDto[];
  /// Hand the run over; `successor` is a device id, or undefined for "any
  /// online device".
  onHandOver?: (successor?: string) => void;
}) {
  const notice = leaseNotice(lease);
  const canHandOver = lease.isMe && lease.health !== "released" && !!onHandOver;
  const extras = (
    <>
      {!lease.verified && !lease.isMe && (
        <span className="text-ink-soft" title="The holder has no certified device key, so its heartbeat is trusted only as far as the workspace key">
          unverified
        </span>
      )}
      {lease.fencedClaims > 0 && (
        <span
          className="rounded px-1.5 py-0.5"
          style={{ color: "var(--ligus-danger-ink)", background: "var(--ligus-danger-tint)" }}
          title="Another device tried to drive this run at the same time; the newer lease epoch won and the other was stopped"
        >
          {lease.fencedClaims} competing claim{lease.fencedClaims === 1 ? "" : "s"} fenced
        </span>
      )}
    </>
  );

  if (!notice) {
    return (
      <div className="mt-2 flex flex-wrap items-center gap-2 text-[11px]" data-testid="run-lease">
        <span className="text-ink-soft">{leaseSummary(lease).text}</span>
        {extras}
        {canHandOver && (
          <LinkButton tone="ink" onClick={() => onHandOver!()} title="Hand this run to another online device of the workspace">
            Hand over
          </LinkButton>
        )}
      </div>
    );
  }

  // Late or released: a neutral note. Lapsed (the driver is gone): warn tint.
  const quiet = lease.health === "released" || lease.health === "degraded";
  return (
    <div
      role="status"
      data-testid="run-lease"
      className="mt-2 rounded-lg border px-2.5 py-2 text-[11.5px] leading-[1.45]"
      style={{
        borderColor: quiet ? "var(--ligus-line)" : "var(--ligus-warn-line)",
        background: quiet ? "var(--ligus-overlay)" : "var(--ligus-warn-tint)",
      }}
    >
      <div className="flex items-start gap-1.5">
        <span aria-hidden className="mt-px inline-flex shrink-0" style={{ color: quiet ? "var(--ligus-ink-soft)" : "var(--ligus-warn-ink)" }}>
          {quiet ? <IconInfo size={13} /> : <IconAlertTriangle size={13} />}
        </span>
        <div className="min-w-0 flex-1">
          <span className="font-semibold">{notice.title}</span>
          <span className="text-ink-soft"> · {notice.detail}</span>
          {(lease.fencedClaims > 0 || (!lease.verified && !lease.isMe)) && (
            <div className="mt-0.5 flex flex-wrap items-center gap-2">{extras}</div>
          )}
        </div>
      </div>
      {canHandOver && <HandOverPicker candidates={handOverCandidates(lease, hosts)} onHandOver={onHandOver!} />}
    </div>
  );
}

function HandOverPicker({
  candidates,
  onHandOver,
}: {
  candidates: WorkspaceHostDto[];
  onHandOver: (successor?: string) => void;
}) {
  // "" = let the app pick any online device.
  const [to, setTo] = useState("");
  if (candidates.length === 0) {
    return (
      <p className="mt-1.5 pl-[19px] text-ink-soft" data-testid="hand-over-none">
        No other device is online to take it. Open Ligus on another device, or cancel the run.
      </p>
    );
  }
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-2 pl-[19px]">
      <label className="flex items-center gap-1.5">
        <span className="text-ink-soft">Hand over to</span>
        <select
          value={to}
          onChange={(e) => setTo(e.target.value)}
          aria-label="Device to hand the run to"
          className="rounded-md border px-1.5 py-0.5 text-[11.5px] outline-none"
          style={fieldStyle}
        >
          <option value="">Any online device</option>
          {candidates.map((h) => (
            <option key={h.id} value={h.id}>
              {h.label || `device ${h.id.slice(0, 8)}`}
            </option>
          ))}
        </select>
      </label>
      <LinkButton onClick={() => onHandOver(to || undefined)}>Hand over</LinkButton>
    </div>
  );
}
