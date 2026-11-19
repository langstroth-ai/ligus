import type { CSSProperties } from "react";
import {
  COMMUNITY_RELAY_DOCS,
  COMMUNITY_RELAY_LABEL,
  type RelayChoice,
} from "@/lib/communityRelay";
import { openExternal } from "@/lib/ipc";

/// Pick where a workspace syncs: the community relay (preselected by the
/// caller), a relay URL the person types, or (when `allowNone`) no relay.
/// The URL travels in invites from here on, so joiners never pick one.
export function RelayPicker({
  choice,
  onChoice,
  ownUrl,
  onOwnUrl,
  allowNone = false,
  inputClassName = "",
  inputStyle,
}: {
  choice: RelayChoice;
  onChoice: (c: RelayChoice) => void;
  ownUrl: string;
  onOwnUrl: (url: string) => void;
  allowNone?: boolean;
  inputClassName?: string;
  inputStyle?: CSSProperties;
}) {
  const options: { v: RelayChoice; title: string; note: string }[] = [
    {
      v: "community",
      title: COMMUNITY_RELAY_LABEL,
      note: "Free, no setup. No uptime guarantee; it sees only ciphertext.",
    },
    { v: "own", title: "Your own relay", note: "A relay you or your organization runs." },
  ];
  if (allowNone) {
    options.push({ v: "none", title: "No relay", note: "This device only; nobody can join until you add a relay." });
  }
  return (
    <div className="space-y-1.5" role="radiogroup" aria-label="Relay">
      {options.map((o) => {
        const active = choice === o.v;
        return (
          <button
            key={o.v}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChoice(o.v)}
            className="flex w-full flex-col items-start gap-0.5 rounded-xl border px-3 py-2 text-left"
            style={{
              borderColor: active ? "var(--ligus-accent-cool)" : "var(--ligus-line)",
              background: active ? "var(--ligus-mist)" : "transparent",
            }}
          >
            <span className="text-sm font-medium">{o.title}</span>
            <span className="text-xs text-ink-soft">{o.note}</span>
          </button>
        );
      })}
      {choice === "community" && (
        <p className="text-xs text-ink-soft">
          Best-effort and may be reset with notice. Self-host a relay for anything important.{" "}
          <button
            type="button"
            className="underline"
            onClick={() => void openExternal(COMMUNITY_RELAY_DOCS).catch(() => {})}
          >
            What the community relay can see
          </button>
        </p>
      )}
      {choice === "own" && (
        <input
          value={ownUrl}
          onChange={(e) => onOwnUrl(e.target.value)}
          placeholder="Relay URL (https base origin, no /v1)"
          aria-label="Relay URL"
          className={inputClassName}
          style={inputStyle}
        />
      )}
    </div>
  );
}
