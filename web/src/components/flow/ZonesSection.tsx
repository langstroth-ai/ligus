import { useState } from "react";
import { useRowMenu } from "@/components/RowMenu";
import { useQuery } from "@tanstack/react-query";
import { Button, EmptyHint, ErrorState, Section, SelectField, LoadingState } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { listZones, removeZone, setZone, type PathZone, type Role, type ZoneKind } from "@/lib/flow";

/// Path zones: core paths carry a high review bar, low-bar paths (docs,
/// tests, generated files) a light one. A change's approval floor and quorum
/// come from where it lands. Admins set them.
export function ZonesSection() {
  const q = useQuery({ queryKey: ["flow-zones"], queryFn: listZones });
  if (q.isError) return <ErrorState text="Couldn't load zones." onRetry={() => void q.refetch()} />;
  if (q.isPending) return <LoadingState rows={2} label="Loading zones" />;
  return (
    <ZonesView
      zones={q.data ?? []}
      onAdd={(z) =>
        setZone(z)
          .then(() => q.refetch())
          .catch((e) => toast.error(`Couldn't add the zone: ${errMsg(e)}`))
      }
      onRemove={(id) =>
        removeZone(id)
          .then(() => q.refetch())
          .catch((e) => toast.error(`Couldn't remove the zone: ${errMsg(e)}`))
      }
    />
  );
}

export function zoneSummary(z: PathZone): string {
  if (z.kind === "core") {
    return `core · ${z.requiredApprovals ?? 2} approval(s) from ${z.approvalFloor ?? "admin"}+`;
  }
  const parts = ["low bar"];
  if (z.requiredApprovals != null) parts.push(`${Math.max(1, z.requiredApprovals)} approval(s)`);
  if (z.approvalFloor) parts.push(`from ${z.approvalFloor}+`);
  return parts.join(" · ");
}

export function ZonesView({
  zones,
  onAdd,
  onRemove,
}: {
  zones: PathZone[];
  onAdd: (z: { pattern: string; kind: ZoneKind; approvalFloor?: Role; requiredApprovals?: number; note: string }) => void;
  onRemove: (id: string) => void;
}) {
  const [pattern, setPattern] = useState("");
  const [kind, setKind] = useState<ZoneKind>("core");
  const [approvals, setApprovals] = useState("");
  return (
    <div className="space-y-4">
      <Section
        title="Zones"
        info="A change touching any core path needs its bar (a core zone only ever raises it). A change whose every file is low-bar may need fewer approvals, never fewer than one. Patterns: a folder (crates/core/), a file, or a glob (**/*.md)."
      >
        {zones.length === 0 ? (
          <EmptyHint text="No zones yet." />
        ) : (
          <ul className="ligus-rowlist text-xs">
            {zones.map((z) => (
              <ZoneItem key={z.id} z={z} onRemove={onRemove} />
            ))}
          </ul>
        )}
      </Section>
      <Section title="Add a zone">
        <input
          aria-label="Pattern"
          placeholder="crates/ligus-core/"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={{ borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
        />
        <SelectField value={kind} onChange={(v) => setKind(v as ZoneKind)} ariaLabel="Zone kind">
          <option value="core">Core (high bar)</option>
          <option value="lowBar">Low bar (docs, tests, generated)</option>
        </SelectField>
        <input
          aria-label="Approvals"
          type="number"
          min={0}
          placeholder={kind === "core" ? "Approvals (default 2)" : "Approvals (optional)"}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={{ borderColor: "var(--ligus-line)", backgroundColor: "var(--ligus-mist)", color: "var(--ligus-ink)" }}
          value={approvals}
          onChange={(e) => setApprovals(e.target.value)}
        />
        <Button
          size="sm"
          disabled={!pattern.trim()}
          onClick={() => {
            onAdd({ pattern: pattern.trim(), kind, requiredApprovals: approvals === "" ? undefined : Number(approvals), note: "" });
            setPattern("");
            setApprovals("");
          }}
        >
          Add zone
        </Button>
      </Section>
    </div>
  );
}

function ZoneItem({ z, onRemove }: { z: Parameters<typeof zoneSummary>[0] & { id: string; pattern: string; note?: string | null }; onRemove: (id: string) => void }) {
  const menu = useRowMenu(`Actions for zone ${z.pattern}`, [{ label: "Remove", danger: true, onSelect: () => onRemove(z.id) }]);
  return (
    <li className="ligus-listrow flex items-center justify-between gap-2 px-1" {...menu.rowProps}>
      <span className="min-w-0 truncate" data-tip={z.note || undefined}>
        <code>{z.pattern}</code> · {zoneSummary(z)}
        {z.note && ` — ${z.note}`}
      </span>
      {menu.button}
      {menu.menu}
    </li>
  );
}
