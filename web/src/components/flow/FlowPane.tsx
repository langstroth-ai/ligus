import { useState, type ReactNode } from "react";
import { SegmentedTabs } from "@/components/ui";
import { RailFrame } from "@/components/RightRail";
import { PANE_DOCS } from "@/lib/docs";
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

let lastFlowTab: FlowTab | null = null;

export function FlowPane({ initialTab = "board" }: { initialTab?: FlowTab }) {
  // Remember the last Flow tab, so leaving the pane and coming back lands where you were.
  const [tab, setTab] = useState<FlowTab>(() => lastFlowTab ?? initialTab);
  const selectTab = (t: FlowTab) => {
    lastFlowTab = t;
    setTab(t);
  };
  return (
    <RailFrame title="Flow" about="Who picks up work, where it waits, and what agents left for each other." docs={PANE_DOCS.flow}>
      <div className="mb-3">
        <SegmentedTabs tabs={TABS} value={tab} onChange={selectTab} label="Flow sections" />
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

/// Small key/value tile used across flow sections.
export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="rounded-lg border px-2.5 py-2" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }} data-tip={hint}>
      <div className="text-[11px] font-semibold uppercase tracking-[0.06em] text-ink-soft">{label}</div>
      <div className="mt-0.5 text-[15px] font-semibold tabular-nums">{value}</div>
    </div>
  );
}

export function Muted({ children }: { children: ReactNode }) {
  return (
    <p className="text-xs leading-[1.5] text-ink-soft">
      {children}
    </p>
  );
}
