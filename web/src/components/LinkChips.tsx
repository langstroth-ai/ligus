import type { LinkChipDto } from "@/lib/ipc";
import { chipColor, chipFor, forgeLabel, stripBidi } from "@/lib/links";

/// A person's linked accounts as small text chips: the forge, the handle and
/// the stable id, and what this device's forge check said. Text only: Ligus
/// never loads a forge's avatar or page (they would tell the forge who looks).
export function LinkChips({ links }: { links: LinkChipDto[] | undefined }) {
  const shown = (links ?? []).map((l) => ({ l, chip: chipFor(l.status) })).filter((x) => x.chip !== null);
  if (shown.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1" aria-label="Linked accounts">
      {shown.map(({ l, chip }) => {
        const c = chipColor(chip!.kind);
        // The link's handle is its publisher's claim: a verified chip shows
        // only the handle the forge confirmed, else the stable id.
        const verified = l.status?.state === "verified";
        const forgeHandle = l.status?.state === "verified" && l.status.handle ? stripBidi(l.status.handle) : "";
        const handle = verified ? forgeHandle || `id ${stripBidi(l.subjectId)}` : stripBidi(l.handle);
        const claimed = stripBidi(l.handle);
        const idNote =
          (l.subjectId && !handle.endsWith(l.subjectId) ? ` (id ${stripBidi(l.subjectId)})` : "") +
          (verified && !forgeHandle && claimed ? ` (the link says @${claimed}; the forge didn't confirm the name)` : "");
        return (
          <span
            key={`${l.provider}/${l.host}/${l.subjectId}`}
            className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px]"
            style={{ borderColor: c.border, color: c.ink }}
            title={`${chip!.title}${idNote}`}
            data-chip={chip!.kind}
          >
            <span className="font-medium">{forgeLabel(l.provider, l.host)}</span>
            <bdi className="font-mono">{handle}</bdi>
            <span className="opacity-70">· {chip!.label}</span>
          </span>
        );
      })}
    </div>
  );
}
