import { useState, type ReactNode } from "react";
import { RailFrame } from "@/components/RightRail";
import { RoutingSection } from "@/components/flow/RoutingSection";
import { QueueSection } from "@/components/flow/QueueSection";
import { MarksSection } from "@/components/flow/MarksSection";
import { ZonesSection } from "@/components/flow/ZonesSection";
import { RosterSection } from "@/components/flow/RosterSection";
import { BoardSection } from "@/components/flow/BoardSection";
import { HomeSection } from "@/components/flow/HomeSection";

/// The Flow pane: how work moves through the workspace — the task board,
/// the review queue and hand-off waits, workspace marks, routing, careers,
/// path zones and the home beacon.
export type FlowTab = "board" | "queue" | "marks" | "routing" | "roster" | "zones" | "home";

const TABS: { id: FlowTab; label: string }[] = [
  { id: "board", label: "Board" },
  { id: "queue", label: "Queue" },
  { id: "marks", label: "Marks" },
  { id: "routing", label: "Routing" },
  { id: "roster", label: "Roster" },
  { id: "zones", label: "Zones" },
  { id: "home", label: "Home" },
];

export function FlowPane({ initialTab = "board" }: { initialTab?: FlowTab }) {
  const [tab, setTab] = useState<FlowTab>(initialTab);
  return (
    <RailFrame title="Flow" subtitle="Who picks up work, where it waits, and what agents left for each other.">
      <div className="mb-3 flex flex-wrap gap-1" role="tablist" aria-label="Flow sections">
        {TABS.map((t) => (
          <TabButton key={t.id} active={tab === t.id} onClick={() => setTab(t.id)}>
            {t.label}
          </TabButton>
        ))}
      </div>
      {tab === "board" && <BoardSection />}
      {tab === "queue" && <QueueSection />}
      {tab === "marks" && <MarksSection />}
      {tab === "routing" && <RoutingSection />}
      {tab === "roster" && <RosterSection />}
      {tab === "zones" && <ZonesSection />}
      {tab === "home" && <HomeSection />}
    </RailFrame>
  );
}

function TabButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className="rounded-lg px-2.5 py-1 text-xs font-medium transition-colors"
      style={{
        background: active ? "var(--hive-mist)" : "transparent",
        color: active ? "var(--hive-accent-cool)" : "var(--hive-ink)",
        opacity: active ? 1 : 0.75,
      }}
    >
      {children}
    </button>
  );
}

/// Small key/value tile used across flow sections.
export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="rounded-xl border px-3 py-2" style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }} title={hint}>
      <div className="text-[10.5px] uppercase tracking-wide opacity-60">{label}</div>
      <div className="text-sm font-semibold">{value}</div>
    </div>
  );
}

export function Muted({ children }: { children: ReactNode }) {
  return (
    <p className="text-xs" style={{ color: "var(--hive-ink-soft)" }}>
      {children}
    </p>
  );
}
