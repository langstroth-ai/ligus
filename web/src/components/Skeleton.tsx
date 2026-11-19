/// Lightweight loading placeholders. Pure CSS pulse (no JS), so they cost
/// nothing to render and disappear the instant real data arrives.

export function Skeleton({ className = "", style }: { className?: string; style?: React.CSSProperties }) {
  return (
    <div
      className={`ligus-skel ${className}`}
      style={style}
    />
  );
}

/// A stack of fake list rows — used for the chat list / member list while the
/// first query is in flight.
export function SkeletonRows({ rows = 5, className = "" }: { rows?: number; className?: string }) {
  return (
    <div className={`space-y-2 ${className}`}>
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="rounded-2xl border px-3 py-3" style={{ borderColor: "var(--ligus-line)" }}>
          <Skeleton className="h-3.5 w-2/3" />
          <Skeleton className="mt-2 h-2.5 w-2/5" />
        </div>
      ))}
    </div>
  );
}

/// Fake transcript turns for the chat loading state — mirrors the flat
/// "Refined" turn layout (avatar, name line, two text lines, hairline between)
/// so load → loaded doesn't visibly jump.
export function SkeletonBubbles({ count = 3 }: { count?: number }) {
  return (
    <div role="status" aria-label="Loading messages">
      {Array.from({ length: count }).map((_, i) => (
        <div key={i} className="tt" style={{ pointerEvents: "none" }}>
          <div className="tt-av">
            <div className={`ligus-skel h-[30px] w-[30px] ${i % 2 ? "rounded-full" : "rounded-[30%]"}`} />
          </div>
          <div className="tt-body space-y-2 pt-1">
            <div className="ligus-skel h-3 w-28" />
            <div className="ligus-skel h-3" style={{ width: `${86 - i * 12}%` }} />
            <div className="ligus-skel h-3" style={{ width: `${58 + i * 9}%` }} />
          </div>
        </div>
      ))}
    </div>
  );
}
