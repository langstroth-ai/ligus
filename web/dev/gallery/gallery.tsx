// Dev-only page that shows the shared kit components in the current theme,
// for design work and the screenshot checks. Open it with `bun run dev:mock`
// at /dev/gallery/ (add ?theme=harbor&mode=dark to switch). Not in the build.
import React, { useState } from "react";
import ReactDOM from "react-dom/client";
import "@/styles.css";
import { applyTheme, resolvePalette, type AppearanceMode, type ThemeName } from "@/lib/theme";
import { Button } from "@/components/ui";
import {
  AvatarStack,
  Combobox,
  Countdown,
  DataTable,
  Fingerprint,
  InboxItem,
  PresenceDot,
  SessionCard,
  SessionStatus,
  Sheet,
  StatusBar,
  StatusBarItem,
  Stepper,
  VirtualList,
} from "@/components/kit";

const q = new URLSearchParams(location.search);
applyTheme(resolvePalette((q.get("theme") ?? "pollen") as ThemeName, (q.get("mode") ?? "light") as AppearanceMode));

function H({ children }: { children: React.ReactNode }) {
  return <h2 className="mb-2 mt-6 text-[11px] font-bold uppercase tracking-[0.08em]" style={{ color: "var(--ligus-ink-soft)" }}>{children}</h2>;
}

const agents = [
  { id: "builder", name: "@builder", host: "Serve box", runs: 14 },
  { id: "scout", name: "@scout", host: "MacBook Pro", runs: 3 },
  { id: "ligus", name: "@ligus", host: "MacBook Pro", runs: 27 },
];

function Gallery() {
  const [host, setHost] = useState<string | null>("mbp");
  const [sheet, setSheet] = useState(q.get("sheet") === "1");
  const t0 = useState(() => Date.now())[0];
  return (
    <div className="min-h-screen" style={{ background: "var(--ligus-canvas)", color: "var(--ligus-ink)" }}>
      <div className="mx-auto max-w-[760px] px-6 pb-16 pt-6">
        <H>Session status and card</H>
        <div className="flex flex-wrap gap-4">
          {(["working", "needsYou", "queued", "done", "failed", "paused"] as const).map((s) => (
            <SessionStatus key={s} state={s} elapsed={s === "working" ? "0:42" : undefined} />
          ))}
        </div>
        <div className="mt-3 grid grid-cols-2 gap-3">
          <SessionCard title="Fix flaky checkout test" state="working" meta="Runs on MacBook Pro · claude-code / opus" detail="Running the suite again" people={<AvatarStack people={[{ name: "Maya", presence: "active" }, { name: "builder", kind: "agent" }]} size={20} />} />
          <SessionCard title="Migrate auth to passkeys" state="needsYou" meta="Runs on Serve box · codex" detail="Waiting on your vote" />
        </div>
        <H>Inbox items</H>
        <div role="list" className="rounded-xl border" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}>
          <InboxItem title="Vote: queue-based scheduler" context="#general" when="2m" detail="2 of 3 approvals; yours completes quorum" unread actions={<Button size="sm">Open</Button>} />
          <InboxItem title="@scout asked a question" context="Relay reconnect backoff" when="14m" detail="Should retries cap at 5 or keep doubling?" />
          <InboxItem title="Run failed: nightly tests" state="failed" context="Workflows" when="1h" />
        </div>
        <H>Stepper and fingerprint</H>
        <Stepper steps={["Name", "Scan", "Confirm", "Check"]} current={2} />
        <div className="mt-3 max-w-[360px]">
          <Fingerprint value="7f3a9c21e0b44d18a6c2" label="Phone fingerprint" />
        </div>
        <H>Combobox and countdown</H>
        <div className="flex items-end gap-6">
          <div className="w-[260px]">
            <Combobox label="Host" value={host} onChange={setHost} options={[
              { value: "mbp", label: "MacBook Pro", hint: "This device" },
              { value: "box", label: "Serve box", hint: "Linux · always on" },
              { value: "ci", label: "CI worker", disabled: true, disabledReason: "Offline since 9:14" },
            ]} />
          </div>
          <Countdown expiresAt={t0 + 245_000} totalMs={300_000} />
        </div>
        <H>Presence</H>
        <div className="flex items-center gap-4">
          <PresenceDot presence="active" name="Maya" />
          <PresenceDot presence="away" name="Zaz" />
          <PresenceDot presence="offline" name="Ada" />
          <AvatarStack people={["Maya", "Zaz", "Ada", "Lin", "Bo", "Kim"].map((name, i) => ({ name, presence: i === 0 ? "active" as const : undefined }))} />
        </div>
        <H>Data table</H>
        <div className="rounded-xl border" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }}>
          <DataTable caption="Agents" rowKey={(r) => r.id} rows={agents} activeKey="scout" onRowClick={() => {}} columns={[
            { key: "name", header: "Agent", render: (r) => r.name, sortValue: (r) => r.name },
            { key: "host", header: "Runs on", render: (r) => r.host, sortValue: (r) => r.host },
            { key: "runs", header: "Runs", render: (r) => r.runs, sortValue: (r) => r.runs, align: "right" },
          ]} />
        </div>
        <H>Virtual list</H>
        <VirtualList className="h-40 rounded-xl border" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)" }} items={Array.from({ length: 5000 }, (_, i) => i)} itemKey={(i) => String(i)} estimateHeight={28} renderItem={(i) => <div className="px-3 py-1 text-[13px]">Log line {i + 1}</div>} />
        <H>Sheet</H>
        <Button onClick={() => setSheet(true)}>Open sheet</Button>
      </div>
      <div className="fixed bottom-0 left-0 right-0">
        <StatusBar start={<><StatusBarItem>Synced</StatusBarItem><StatusBarItem>Relay: relay.example.com</StatusBarItem></>} end={<StatusBarItem tone="info">2 agents running</StatusBarItem>} />
      </div>
      {sheet && (
        <Sheet title="Add a phone" description="Pair a phone to this workspace." steps={["Name", "Scan", "Confirm", "Check"]} step={1} onClose={() => setSheet(false)} footer={<><Button variant="quiet" onClick={() => setSheet(false)}>Back</Button><Button variant="primary">Next</Button></>}>
          <p className="text-[14px]">Scan the code with the Ligus app on the phone.</p>
        </Sheet>
      )}
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(<Gallery />);
