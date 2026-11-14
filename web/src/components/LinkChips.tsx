import type { LinkChipDto } from "@/lib/ipc";
import { chipColor, chipFor, forgeLabel } from "@/lib/links";

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
        const handle = l.status?.state === "verified" && l.status.handle ? l.status.handle : l.handle;
        const idNote = l.subjectId && l.subjectId !== handle ? ` (id ${l.subjectId})` : "";
        return (
          <span
            key={`${l.provider}/${l.host}/${l.subjectId}`}
            className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px]"
            style={{ borderColor: c.border, color: c.ink }}
            title={`${chip!.title}${idNote}`}
            data-chip={chip!.kind}
          >
            <span className="font-medium">{forgeLabel(l.provider, l.host)}</span>
            <span className="font-mono">{handle}</span>
            <span className="opacity-70">· {chip!.label}</span>
          </span>
        );
      })}
    </div>
  );
}
