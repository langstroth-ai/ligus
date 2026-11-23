import { Button } from "@/components/ui";
import { IconMessage, IconPlus } from "@/lib/icons";
import { Kbd } from "@/components/Tooltip";

/// A faint honeycomb: three comb cells, the one bee note on an otherwise
/// quiet surface (Ligus is short for ligustica, the Italian honeybee).
function CombMark() {
  const cell = "M0 -20 L17.3 -10 L17.3 10 L0 20 L-17.3 10 L-17.3 -10 Z";
  return (
    <svg width="96" height="84" viewBox="-40 -36 80 72" aria-hidden className="text-accent">
      <g fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round">
        <path d={cell} transform="translate(-18 -15)" opacity="0.55" />
        <path d={cell} transform="translate(18 -15)" opacity="0.35" />
        <path d={cell} transform="translate(0 15)" opacity="0.8" fill="color-mix(in srgb, currentColor 14%, transparent)" />
      </g>
    </svg>
  );
}

/// The canvas when no chat is open: what this place is and the one next step,
/// plus the keyboard routes in.
export function EmptyCanvas({
  workspaceName,
  onNewChat,
  onOpenPalette,
}: {
  workspaceName: string;
  onNewChat: () => void;
  onOpenPalette: () => void;
}) {
  return (
    <div className="ligus-enter flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
      <CombMark />
      <h2 className="ligus-title">Nothing open in {workspaceName}</h2>
      <p className="max-w-[360px] text-[13px] leading-[1.55] text-ink-soft">
        Start a chat and @-mention an agent to bring it in, or pick up a conversation from the sidebar.
      </p>
      <div className="mt-1 flex flex-wrap justify-center gap-2">
        <Button variant="primary" size="md" onClick={onNewChat}>
          <IconPlus size={14} /> New chat
        </Button>
        <Button size="md" onClick={onOpenPalette}>
          <IconMessage size={14} /> Jump to…
        </Button>
      </div>
      <p className="mt-2 flex flex-wrap items-center justify-center gap-x-3 gap-y-1 text-xs text-ink-soft">
        <span>
          <Kbd>⌘K</Kbd> anything
        </span>
        <span>
          <Kbd>⌘⇧]</Kbd> next chat
        </span>
        <span>
          <Kbd>⌘J</Kbd> tools rail
        </span>
      </p>
    </div>
  );
}

/// Placeholder while a lazy canvas (Monaco, xterm) loads: the frame of the
/// view, not a bare line of text, so the swap doesn't jump.
export function CanvasLoading({ label }: { label: string }) {
  return (
    <div role="status" aria-label={label} className="flex h-full min-h-0">
      <div className="w-56 shrink-0 space-y-2 border-r p-3" style={{ borderColor: "var(--ligus-line)" }}>
        {[70, 55, 80, 45, 60].map((w, i) => (
          <div key={i} className="ligus-skel h-3" style={{ width: `${w}%` }} />
        ))}
      </div>
      <div className="min-w-0 flex-1 space-y-2.5 p-4">
        {[90, 75, 82, 60, 88, 40, 70].map((w, i) => (
          <div key={i} className="ligus-skel h-3" style={{ width: `${w}%` }} />
        ))}
      </div>
    </div>
  );
}
