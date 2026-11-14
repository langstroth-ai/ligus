import type { RunLeaseDto } from "@/bindings/RunLeaseDto";

/// One-line summary of a workflow run's coordinator lease: who drives it and
/// whether that device is still beating. Exported for tests.
export function leaseSummary(lease: RunLeaseDto): { text: string; tone: "ok" | "warn" | "bad" } {
  const who = lease.isMe ? "this device" : lease.holderLabel || `device ${lease.holder.slice(0, 8)}`;
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

const TONE: Record<string, string> = {
  ok: "var(--hive-success)",
  warn: "var(--hive-warning, #b7791f)",
  bad: "var(--hive-danger)",
};

export function RunLeaseLine({
  lease,
  onHandOver,
}: {
  lease: RunLeaseDto;
  onHandOver?: () => void;
}) {
  const { text, tone } = leaseSummary(lease);
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2 text-[11px]" data-testid="run-lease">
      <span style={{ color: TONE[tone] }}>●</span>
      <span className="text-ink-soft">{text}</span>
      {!lease.verified && !lease.isMe && (
        <span className="text-ink-soft" title="The holder has no certified device key, so its heartbeat is trusted only as far as the workspace key">
          unverified
        </span>
      )}
      {lease.fencedClaims > 0 && (
        <span
          className="rounded px-1.5 py-0.5"
          style={{ color: "var(--hive-danger-ink)", background: "color-mix(in srgb, var(--hive-danger) 12%, transparent)" }}
          title="Another device tried to drive this run at the same time; the newer lease epoch won and the other was stopped"
        >
          {lease.fencedClaims} competing claim{lease.fencedClaims === 1 ? "" : "s"} fenced
        </span>
      )}
      {lease.isMe && lease.health !== "released" && onHandOver && (
        <button
          type="button"
          className="rounded-lg px-2 py-0.5 font-medium underline-offset-2 hover:underline"
          onClick={onHandOver}
          title="Hand this run to another online device of the workspace"
        >
          Hand over
        </button>
      )}
    </div>
  );
}
