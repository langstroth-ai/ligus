// Canned data for the dev-only mock backend (dev/mock/core.ts).
const now = Date.now();
const iso = (minsAgo: number) => new Date(now - minsAgo * 60_000).toISOString();

export const settings = (onboarding: boolean) => ({
  displayName: onboarding ? "" : "Zaz",
  gitEmail: "zaz@example.com",
  deviceName: "MacBook Pro",
  workspaceRoot: "/Users/zaz/projects/apiary",
  workspaceFolder: "/Users/zaz/projects/apiary",
  knownWorkspaces: ["/Users/zaz/projects/apiary"],
  model: "claude-code",
  gitBranch: "main",
  gitDirtyCount: 3,
  avatarUrl: null,
  onboarded: !onboarding,
  tourSeen: true,
});

export const workspaces = [
  { id: "ws-1", name: "Apiary", kind: "room", active: true, iconUrl: null },
  { id: "ws-local", name: "My workspace", kind: "local", active: false, iconUrl: null },
  { id: "ws-2", name: "Field notes", kind: "room", active: false, iconUrl: null },
];

export const runtimes = [
  { id: "rt-claude", name: "claude-code", label: "Claude Code", provider: "claude-code", location: "local", model: "opus", endpoint: "", supportsTools: true, supportsEmbeddings: false, isDefault: true, isManaged: false, modelBaseUrl: null, modelProviderId: null, contextWindow: null },
  { id: "rt-ollama", name: "ollama", label: "Ollama · qwen3-coder", provider: "ollama", location: "local", model: "qwen3-coder:30b", endpoint: "http://localhost:11434", supportsTools: true, supportsEmbeddings: true, isDefault: false, isManaged: false, modelBaseUrl: null, modelProviderId: null, contextWindow: 32768 },
  { id: "rt-codex", name: "codex", label: "Codex", provider: "codex", location: "local", model: "gpt-5-codex", endpoint: "", supportsTools: true, supportsEmbeddings: false, isDefault: false, isManaged: false, modelBaseUrl: null, modelProviderId: null, contextWindow: null },
];

export const channels = [
  { id: "ch-general", name: "general", purpose: "Anything goes", position: 0, archived: false, defaultChatId: "chat-general" },
  { id: "ch-sync", name: "sync-engine", purpose: "Relay + E2EE work", position: 1, archived: false, defaultChatId: "chat-sync" },
];

export const chats = [
  { id: "chat-1", title: "Hive inspection scheduler", lastActivityAt: iso(2), messageCount: 9, archived: false, channelId: "ch-general", isChannelDefault: false },
  { id: "chat-general", title: "general", lastActivityAt: iso(60), messageCount: 3, archived: false, channelId: "ch-general", isChannelDefault: true },
  { id: "chat-sync", title: "sync-engine", lastActivityAt: iso(300), messageCount: 2, archived: false, channelId: "ch-sync", isChannelDefault: true },
  { id: "chat-2", title: "Relay reconnect backoff", lastActivityAt: iso(120), messageCount: 14, archived: false, channelId: "ch-sync", isChannelDefault: false },
  { id: "chat-3", title: "Varroa mite count import", lastActivityAt: iso(1440), messageCount: 6, archived: false, channelId: "", isChannelDefault: false },
];

const msg = (o: Partial<Record<string, unknown>> & { id: string; role: string; author: string; body: string; mins: number }) => ({
  id: o.id,
  role: o.role,
  author: o.author,
  body: o.body,
  isStreaming: (o.isStreaming as boolean) ?? false,
  createdAt: iso(o.mins),
  reactions: o.reactions ?? [],
  toolCalls: o.toolCalls ?? [],
  toolResults: o.toolResults ?? [],
  authorAvatarUrl: null,
  authorColorHex: null,
  responderName: o.responderName ?? null,
  runtimeId: o.runtimeId ?? null,
  runtimeLabel: o.runtimeLabel ?? null,
});

const messages = [
  msg({ id: "m0", role: "system", author: "system", body: "Maya joined the chat", mins: 40 }),
  msg({
    id: "m1",
    role: "user",
    author: "Zaz",
    body: "@scout can you sketch an inspection scheduler? Colonies need a check every 7-10 days in season, and we should skip rainy days.",
    mins: 38,
    reactions: [{ emoji: "🐝", actorId: "a-maya", actorDisplayName: "Maya" }],
  }),
  msg({
    id: "m2",
    role: "agent",
    author: "scout",
    runtimeId: "rt-claude",
    runtimeLabel: "claude-code / opus",
    mins: 36,
    body:
      "Here's a first pass. The scheduler picks the **next dry day** inside each colony's window:\n\n" +
      "1. Compute the window `[last + 7d, last + 10d]`\n2. Drop days with rain probability over 40%\n3. Pick the earliest remaining day, else the window end\n\n" +
      "```rust\npub fn next_inspection(last: NaiveDate, forecast: &[Day]) -> NaiveDate {\n    let window = (last + Days::new(7))..=(last + Days::new(10));\n    forecast\n        .iter()\n        .filter(|d| window.contains(&d.date) && d.rain < 0.4)\n        .map(|d| d.date)\n        .next()\n        .unwrap_or(*window.end())\n}\n```\n\n" +
      "| Colony | Last check | Next |\n|---|---|---|\n| Ligustica A | Nov 2 | Nov 10 |\n| Ligustica B | Nov 5 | Nov 13 |\n\n> Rain data comes from the forecast feed already wired into `weather.rs`.",
    toolCalls: [
      { id: "tc1", name: "read_file", inputJson: '{"path":"src/weather.rs"}', serverId: null },
      { id: "tc2", name: "run_command", inputJson: '{"cmd":"cargo test -p apiary scheduler"}', serverId: null },
    ],
    toolResults: [
      { callId: "tc1", content: "pub struct Day { pub date: NaiveDate, pub rain: f32 }", isError: false },
      { callId: "tc2", content: "test result: ok. 4 passed; 0 failed", isError: false },
    ],
  }),
  msg({
    id: "m3",
    role: "user",
    author: "Maya",
    body: "Nice. Can we also block inspections below 15°C? Opening the hive when it is cold chills the brood.",
    mins: 20,
  }),
  msg({
    id: "m4",
    role: "agent",
    author: "builder",
    runtimeId: "rt-codex",
    runtimeLabel: "codex / gpt-5-codex",
    mins: 12,
    body: "Added a temperature floor and filed the change for review. Handing the test pass to @scout.",
    toolCalls: [{ id: "tc3", name: "apply_patch", inputJson: '{"files":["src/scheduler.rs"]}', serverId: null }],
    toolResults: [{ callId: "tc3", content: "error: patch did not apply cleanly at src/scheduler.rs:41", isError: true }],
  }),
  msg({
    id: "m5",
    role: "assistant",
    author: "ligus",
    responderName: "Zaz",
    runtimeId: "rt-ollama",
    runtimeLabel: "ollama / qwen3-coder",
    mins: 2,
    body: "Re-applied against the latest `main`; the patch is in the Review pane as **Cold-day floor for inspections**.",
  }),
];

export const chatSession = (id: string) => ({
  id: id || "chat-1",
  title: chats.find((c) => c.id === id)?.title ?? "Hive inspection scheduler",
  runtimeId: "rt-claude",
  messages: id === "chat-1" || !id ? messages : messages.slice(0, 2),
});

export const telemetry = (sid: string) => ({
  sessionId: sid,
  runtimeId: "rt-claude",
  model: "opus",
  contextWindowTokens: 200000,
  reservedOutputTokens: 16000,
  summaryReserveTokens: 4000,
  systemPromptTokens: 3200,
  historyBudgetTokens: 176800,
  historyTokens: 42000,
  messageCount: 9,
  keptMessageCount: 9,
  keptTokens: 42000,
  overflowMessageCount: 0,
  overflowTokens: 0,
  skillCount: 2,
  skillTokens: 900,
  vaultCount: 1,
  summaryStrategy: "rolling",
});

export const syncStatus = {
  relayConfigured: true,
  relayUrl: "https://relay.example.org",
  room: "apiary",
  encrypted: true,
  connectionState: "live",
  lastError: null,
};

const patch = `@@ -38,6 +38,10 @@ pub fn next_inspection(last: NaiveDate, forecast: &[Day]) -> NaiveDate {
     let window = (last + Days::new(7))..=(last + Days::new(10));
     forecast
         .iter()
-        .filter(|d| window.contains(&d.date) && d.rain < 0.4)
+        .filter(|d| window.contains(&d.date))
+        .filter(|d| d.rain < 0.4)
+        // Opening the hive below 15°C chills the brood.
+        .filter(|d| d.high_c >= 15.0)
         .map(|d| d.date)
         .next()
         .unwrap_or(*window.end())
`;

export const diffs = [
  { path: "src/scheduler.rs", kind: "modified", patch, addedLines: 4, removedLines: 1 },
  { path: "src/weather.rs", kind: "modified", patch: "@@ -1,3 +1,4 @@\n pub struct Day {\n     pub date: NaiveDate,\n     pub rain: f32,\n+    pub high_c: f32,\n }\n", addedLines: 1, removedLines: 0 },
  { path: "tests/scheduler_cold.rs", kind: "untracked", patch: "@@ -0,0 +1,3 @@\n+#[test]\n+fn skips_cold_days() {}\n+\n", addedLines: 3, removedLines: 0 },
];

const access = (o: Partial<{ access: string; memberTurnsPerDay: number | null; effectiveMemberTurnsPerDay: number | null; usdCentsPerDay: number | null; notifyOnUse: boolean; available: boolean; unavailableReason: string | null; canManageAccess: boolean }>) => ({
  access: "owner",
  memberTurnsPerDay: null,
  effectiveMemberTurnsPerDay: 50,
  usdCentsPerDay: null,
  notifyOnUse: false,
  available: true,
  unavailableReason: null,
  canManageAccess: false,
  ...o,
});

export const agents = [
  { id: "ag-scout", name: "scout", runtimeId: "rt-claude", role: "Explores options and reviews", ownerActorId: "a-zaz", hostId: "", avatarUrl: null, avatarColorHex: null, ...access({ canManageAccess: true, notifyOnUse: true }) },
  { id: "ag-builder", name: "builder", runtimeId: "rt-codex", role: "Implements changes", ownerActorId: "a-maya", hostId: "h-box", avatarUrl: null, avatarColorHex: "#4a8c48", ...access({ available: false, unavailableReason: "Only its owner can use this agent" }) },
  { id: "ag-ligus", name: "ligus", runtimeId: "rt-ollama", role: "Default assistant", ownerActorId: "a-zaz", hostId: "", avatarUrl: null, avatarColorHex: null, ...access({ access: "everyone", canManageAccess: true, usdCentsPerDay: 500 }) },
];

export const agentUsage = (agentId: string) => ({
  agentId,
  day: "2026-11-20",
  today: [
    { member: "a-zaz", memberName: "Zaz", turns: 6, uncostedTurns: 0, estimatedCostUsd: 0.42 },
    { member: "a-maya", memberName: "Maya", turns: 3, uncostedTurns: 3, estimatedCostUsd: null },
  ],
  month: [
    { member: "a-zaz", memberName: "Zaz", turns: 41, uncostedTurns: 2, estimatedCostUsd: 3.17 },
    { member: "a-maya", memberName: "Maya", turns: 12, uncostedTurns: 12, estimatedCostUsd: null },
  ],
});

export const members = [
  { id: "mem-zaz", actorId: "a-zaz", displayName: "Zaz", role: "owner", title: "Beekeeper", index: 1, isSelf: true, avatarUrl: null },
  { id: "mem-maya", actorId: "a-maya", displayName: "Maya", role: "admin", title: "Backend", index: 2, isSelf: false, avatarUrl: null },
  { id: "mem-theo", actorId: "a-theo", displayName: "Theo", role: "contributor", title: "Design", index: 3, isSelf: false, avatarUrl: null },
  { id: "mem-iris", actorId: "a-iris", displayName: "Iris", role: "viewer", title: "", index: 4, isSelf: false, avatarUrl: null },
];

export const serverMembers = [
  { account: "ligus:6f1c2d3e-8a7b-4c1d-9e0f-112233445566", login: "", role: "owner", addedBy: "", addedAt: 0 },
  { account: "ligus:a1b2c3d4-0000-4000-8000-000000000002", login: "", role: "admin", addedBy: "ligus:6f1c2d3e-8a7b-4c1d-9e0f-112233445566", addedAt: 0 },
];

export const presence = [{ actorId: "a-maya", name: "Maya", typing: false, sessionId: "chat-1" }];

const approval = (name: string, role: string, approved: boolean) => ({ actorId: `a-${name.toLowerCase()}`, displayName: name, role, approved });

export const proposals = [
  {
    id: "p1",
    title: "Cold-day floor for inspections",
    body: "**Skip cold-day inspections so the brood is not chilled.**\n\n**Context**\n\n- Every Monday the scheduler books an inspection, whatever the weather.\n- Opening a hive below 15°C chills the brood.\n\nSee the [scheduler notes](https://example.org/notes) and `src/weather.rs`.",
    authorActorId: "ag-builder",
    kind: "fileDiff",
    status: "open",
    requiredApprovals: 2,
    qualifyingApprovals: 1,
    quorumMet: false,
    approvals: [approval("Maya", "admin", true)],
    diff: `diff --git a/src/scheduler.rs b/src/scheduler.rs\n--- a/src/scheduler.rs\n+++ b/src/scheduler.rs\n${patch}`,
    changedFiles: ["src/scheduler.rs", "src/weather.rs"],
    createdAt: iso(10),
    dismissed: false,
    refinements: [{ id: "r1", actorId: "a-theo", displayName: "Theo", role: "contributor", text: "Should 15°C be configurable per apiary?", requestChanges: false, createdAt: iso(6) }],
    setId: null,
  },
  {
    id: "p2",
    title: "Run cargo fmt before commits",
    body: "Add a **pre-commit** step:\n\n1. `cargo fmt --check`\n2. fail the commit on a diff",
    authorActorId: "ag-scout",
    kind: "command",
    status: "approved",
    requiredApprovals: 1,
    qualifyingApprovals: 1,
    quorumMet: true,
    approvals: [approval("Zaz", "owner", true)],
    changedFiles: [],
    createdAt: iso(200),
    dismissed: false,
    refinements: [],
  },
  {
    id: "p3",
    title: "Switch forecast provider",
    body: "Decision only.",
    authorActorId: "a-maya",
    kind: "decision",
    status: "rejected",
    requiredApprovals: 1,
    qualifyingApprovals: 0,
    quorumMet: false,
    approvals: [approval("Zaz", "owner", false)],
    changedFiles: [],
    createdAt: iso(900),
    dismissed: false,
    refinements: [],
  },
  { id: "pa", title: "Queue-based scheduler", body: "", authorActorId: "ag-scout", kind: "fileDiff", status: "open", requiredApprovals: 1, qualifyingApprovals: 0, quorumMet: false, approvals: [], changedFiles: ["src/scheduler.rs"], createdAt: iso(30), dismissed: false, refinements: [], setId: "set-1" },
  { id: "pb", title: "Cron-style scheduler", body: "", authorActorId: "ag-builder", kind: "fileDiff", status: "open", requiredApprovals: 1, qualifyingApprovals: 0, quorumMet: false, approvals: [], changedFiles: ["src/scheduler.rs", "Cargo.toml"], createdAt: iso(29), dismissed: false, refinements: [], setId: "set-1" },
];

const assess = (id: string, name: string, rt: string, score: number, rationale: string) => ({ id, scoutName: name, scoutRuntime: rt, score, currentSupport: score, rationale, round: 1, isSelf: false, createdAt: iso(20) });

export const proposalSets = [
  {
    id: "set-1",
    question: "How should we schedule inspections?",
    body: "Two takes on the scheduler; scouts assess each.",
    author: "Zaz",
    taskClass: "feature",
    phase: "scouting",
    round: 1,
    attempt: 1,
    agentQuorum: 2,
    qualityBar: 0.6,
    decayPerRound: 0.1,
    requiredApprovals: 1,
    approvalRoleFloor: "contributor",
    dial: "balanced",
    scoutingDeadline: new Date(now + 25 * 60_000).toISOString(),
    deadlinePassed: false,
    frontRunner: "pa",
    tieBreak: false,
    winner: null,
    decidedAt: null,
    createdAt: iso(30),
    alternatives: [
      {
        proposalId: "pa", label: "A", title: "Queue-based scheduler", body: "A priority queue keyed on next-due date; cheap to recompute when the forecast changes.", kind: "fileDiff", status: "open",
        changedFiles: ["src/scheduler.rs"], support: 0.78, qualifyingScouts: 2, scouts: 2, runtimes: 2, meetsQuorum: true, frozen: false, active: true, superseded: false, stopReason: null, stoppedBy: null,
        assessments: [assess("as1", "scout", "claude-code", 0.82, "Simple, testable, and handles forecast churn."), assess("as2", "builder", "codex", 0.74, "Good; watch heap growth with many apiaries.")],
        pendingScouts: [], qualifyingApprovals: 0, requiredApprovals: 1,
        preflight: { passed: true, tests: { status: "passed", note: "14 passed" }, baseUnchanged: { status: "passed", note: "main unchanged" }, hostsOnline: { status: "passed", note: "2 of 2 online" }, at: iso(5) },
        reverted: false,
      },
      {
        proposalId: "pb", label: "B", title: "Cron-style scheduler", body: "Expresses each colony's cadence as a cron rule; familiar but awkward for weather skips.", kind: "fileDiff", status: "open",
        changedFiles: ["src/scheduler.rs", "Cargo.toml"], support: 0.41, qualifyingScouts: 1, scouts: 2, runtimes: 1, meetsQuorum: false, frozen: false, active: true, superseded: false, stopReason: null, stoppedBy: null,
        assessments: [assess("as3", "scout", "claude-code", 0.41, "Adds a dependency and fights the rain-skip rule.")],
        pendingScouts: ["builder"], qualifyingApprovals: 0, requiredApprovals: 1, preflight: null, reverted: false,
      },
    ],
  },
];

export const skills = [{ id: "sk1", name: "rust-style", instructions: "Prefer iterators.", sourceUrl: null, agentIds: [] }];
export const mcpServers = [
  { id: "fs", transport: "stdio", detail: "ligus-fs", enabled: true, isManaged: true, hasBearerToken: false, oauthConnected: false, secretForOtherUrl: false },
  { id: "linear", transport: "http", detail: "https://mcp.linear.app", enabled: false, isManaged: false, hasBearerToken: true, oauthConnected: false, secretForOtherUrl: false },
];
export const vaults = [{ kind: "git", label: "beekeeping-notes", url: "https://github.com/example/notes", agentIds: [] }];

export const workflows = [
  {
    id: "wf1",
    name: "Ship a change",
    description: "Implement, review, then gate on a human before merge.",
    inputLabel: "What should change?",
    disabled: false,
    nodes: [
      { id: "n1", name: "Implement", dependsOn: [], kind: "agent", agentId: "ag-builder", promptTemplate: "{{input}}", gateTitle: null, gateBody: null, requiredApprovals: null, onReject: null, rejectTarget: null },
      { id: "n2", name: "Review", dependsOn: ["n1"], kind: "agent", agentId: "ag-scout", promptTemplate: "Review the change", gateTitle: null, gateBody: null, requiredApprovals: null, onReject: null, rejectTarget: null },
      { id: "n3", name: "Human sign-off", dependsOn: ["n2"], kind: "gate", agentId: null, promptTemplate: null, gateTitle: "Merge?", gateBody: null, requiredApprovals: 1, onReject: "retry", rejectTarget: "n1" },
    ],
  },
];

export const workflowRuns = [
  {
    id: "run1", definitionId: "wf1", definitionName: "Ship a change", input: "Add a cold-day floor", status: "running", startedAt: iso(14), driverAlive: true,
    nodes: [
      { nodeId: "n1", name: "Implement", kind: "agent", status: "done", messageId: null, proposalId: null, outputExcerpt: "Patched scheduler.rs", attempts: 1, error: "" },
      { nodeId: "n2", name: "Review", kind: "agent", status: "running", messageId: null, proposalId: null, outputExcerpt: "", attempts: 1, error: "" },
      { nodeId: "n3", name: "Human sign-off", kind: "gate", status: "pending", messageId: null, proposalId: null, outputExcerpt: "", attempts: 0, error: "" },
    ],
    lease: { holder: "h-mac", holderLabel: "MacBook Pro", isMe: true, epoch: 3, leaseMs: 30000, health: "degraded", lastBeatAgeMs: 21000, verified: true, handoverTo: null, fencedClaims: 0 },
  },
];

export const queuedWork = [{ sessionId: "chat-2", chatTitle: "Relay reconnect backoff", agentId: "ag-builder", agentName: "builder", hostId: "h-box", hostLabel: "serve box", hostStatus: "offline" }];
export const hosts = [
  { id: "h-mac", kind: "desktop", label: "MacBook Pro", online: true, lastSeen: iso(0) },
  { id: "h-box", kind: "worker", label: "serve box", online: false, lastSeen: iso(95) },
  { id: "h-mara", kind: "device", label: "Mara's laptop", online: true, lastSeen: iso(0) },
];

export const dearth = (enabled: boolean) => ({
  enabled,
  reason: enabled ? "robbing season" : "",
  broodPaths: [".github/**", "Cargo.toml"],
  allowedMcpServers: ["fs"],
  setBy: "Zaz",
  setAt: iso(50),
  canManage: true,
  defaultBroodPaths: [".github/**", "**/*.lock"],
  pending: enabled ? [{ subject: "github:9", kind: "member", name: "drone-42", addedBy: "Maya", canApprove: true }] : [],
  entrances: [
    { name: "Invites", kind: "invite", state: enabled ? "two approvals" : "open" },
    { name: "Devices", kind: "device", state: enabled ? "two approvals" : "open" },
    { name: "MCP network", kind: "mcp", state: enabled ? "allow-list" : "open" },
  ],
});

export const lockdown = (active: boolean) => ({
  active,
  signals: active ? [{ signal: "injectionDetected", detail: "fetch_url result", raisedAt: iso(3) }] : [],
  canClear: true,
});

export const guardSignals = [
  { rowId: 1, atMs: now - 180_000, kind: "lockdown.injectionDetected", sessionId: "chat-1", attrsJson: '{"tool":"fetch_url"}' },
  { rowId: 2, atMs: now - 3_600_000, kind: "dearth.join_held", sessionId: null, attrsJson: '{"subject":"github:9"}' },
];

export const hygiene = { ranAt: iso(240), repoBound: true, turnWorktreesRemoved: 4, chatCheckoutsRemoved: 1, parkedWorktreesRemoved: 0, staleProposals: [{ sessionId: "chat-3", proposalId: "px", title: "Old varroa importer" }] };

export const throttle = {
  backgroundPct: 60,
  workflowPct: 80,
  staggerPct: 10,
  windows: [
    { runtime: "claude-code", status: "ok", effectivePct: 42, reported: true, window: "5h", resetsAt: new Date(now + 2 * 3_600_000).toISOString(), ageSecs: 120 },
    { runtime: "codex", status: "throttled", effectivePct: 86, reported: true, window: "weekly", resetsAt: new Date(now + 40 * 3_600_000).toISOString(), ageSecs: 600 },
  ],
  deferred: [{ priority: "background", what: "Nightly hygiene summary", runtime: "codex", utilizationPct: 86, thresholdPct: 60 }],
};

export const growth = [{ id: "g1", kind: "agent", title: "Add a second reviewer runtime", reason: "Every review so far ran on one provider." }];

export const flowSettings = {
  handoffCards: true,
  routeMode: "shadow",
  epsilonMin: 0.05,
  epsilonMax: 0.3,
  halfLifeDays: 14,
  cooldownMinutes: 30,
  stopUpstream: "on",
  queueLimit: 6,
  longWaitMinutes: 45,
  recruitApprovers: true,
  offerIdleHost: true,
  marksInContext: true,
  boardAutoPickup: false,
  reuseWorktrees: true,
};

export const handoffs = [
  {
    id: "ho1", from: "ag-builder", to: "ag-scout", fromName: "builder", toName: "scout", messageId: "m4",
    task: "Write tests for the cold-day floor", taskClass: "test", branch: "ligus/chat/chat-1", files: ["src/scheduler.rs", "tests/scheduler_cold.rs"],
    quality: { testsPassed: false, proposalId: "p1", proposalStatus: "open", turns: 3, costUsd: 0.42, error: true },
    via: "mention",
    parcel: { keyFiles: ["src/scheduler.rs"], decisions: ["Floor is 15°C, not configurable yet"], ruledOut: ["Hourly forecast granularity"], notes: "Patch failed once on line 41; rebased." },
    atMs: now - 12 * 60_000,
  },
];

export const routing = {
  mode: "shadow",
  sinceMs: now - 14 * 86_400_000,
  decisions: 48,
  offPrimaryShare: 0.27,
  scores: [
    { agent: "ag-scout", agentName: "scout", taskClass: "review", score: 0.81, recent: 0.85, weight: 12, outcomes: 14, successes: 12, turnsPerSuccess: 2.1 },
    { agent: "ag-builder", agentName: "builder", taskClass: "feature", score: 0.66, recent: 0.6, weight: 9, outcomes: 11, successes: 7, turnsPerSuccess: 3.4 },
  ],
  arms: [
    { arm: "claude-code", turns: 60, proposals: 12, accepted: 9, judged: 11, acceptanceRate: 0.82, turnsPerAccepted: 6.7, costUsd: 4.1, costPerAccepted: 0.46, localShare: 0 },
    { arm: "ollama", turns: 31, proposals: 6, accepted: 3, judged: 5, acceptanceRate: 0.6, turnsPerAccepted: 10.3, costUsd: 0, costPerAccepted: 0, localShare: 1 },
  ],
  recent: [
    { atMs: now - 600_000, sessionId: "chat-1", taskClass: "test", mode: "shadow", chosen: "ag-scout", wouldPick: "ag-scout", preferred: "ag-scout", explore: false, reason: "threadMemory", epsilon: 0.1 },
    { atMs: now - 3_000_000, sessionId: "chat-2", taskClass: "feature", mode: "shadow", chosen: "ag-builder", wouldPick: "ag-ligus", preferred: "ag-builder", explore: true, reason: "explore", epsilon: 0.22 },
  ],
  stops: [{ route: "ag-builder:feature", name: "builder · feature", reason: "3 failed turns in a row", untilMs: now + 18 * 60_000, failures: 3 }],
  exploration: { explored: 9, share: 0.19, meanEpsilon: 0.14, exploredWins: 4, exploredJudged: 7, incumbentRate: 0.71 },
};

export const queue = {
  gate: { open: 7, oldestWaitMs: 52 * 60_000, medianWaitMs: 18 * 60_000, trembleShare: 0.35, openedLastHour: 5, decidedLastHour: 2, overLimit: true },
  edges: [
    { edge: "gate", samples: 22, medianWaitMs: 18 * 60_000, trembleShare: 0.35 },
    { edge: "handoff", samples: 9, medianWaitMs: 4 * 60_000, trembleShare: 0.1 },
  ],
  alert: "Review queue is over its limit (7 of 6). New agent work is paused upstream.",
  queueLimit: 6,
  stopUpstream: "on",
  recruits: [{ id: "rc1", action: "approvers", queueLen: 7, oldestWaitMs: 52 * 60_000, memberIds: ["mem-theo"], atMs: now - 300_000 }],
};

export const marks = [
  { id: "mk1", kind: "claim", path: "src/scheduler.rs", note: "Adding the cold floor", agent: "builder", by: "Maya", sessionId: "chat-1", atMs: now - 900_000, expiresMs: now + 3_600_000, chatTitle: "Hive inspection scheduler" },
  { id: "mk2", kind: "needsReview", path: "src/relay/backoff.rs", note: "Jitter math", agent: "", by: "Theo", sessionId: "chat-2", atMs: now - 7_200_000, chatTitle: "Relay reconnect backoff" },
];

export const zones = [
  { id: "z1", pattern: "crates/crypto/**", kind: "core", approvalFloor: "admin", requiredApprovals: 2, note: "E2EE code", atMs: now - 86_400_000 },
  { id: "z2", pattern: "docs/**", kind: "lowBar", note: "Docs can merge on one approval", atMs: now - 86_400_000 },
];

export const roster = {
  agents: [
    { agentId: "ag-scout", name: "scout", runtimeId: "rt-claude", career: "reviewer", why: "14 judged, 86% approved", judged: 14, approved: 12, profile: null },
    { agentId: "ag-builder", name: "builder", runtimeId: "rt-codex", career: "proposer", why: "11 judged, 64% approved", judged: 11, approved: 7, profile: null },
    { agentId: "ag-ligus", name: "ligus", runtimeId: "rt-ollama", career: "probation", why: "2 judged so far", judged: 2, approved: 1, profile: null },
  ],
  diversity: { agents: 3, runtimes: 3, providers: 3, monoculture: false, advice: "" },
};

export const board = [
  { id: "b1", kind: "failingTests", title: "scheduler_cold::skips_cold_days fails", sessionId: "chat-1", files: ["tests/scheduler_cold.rs"], sinceMs: now - 600_000, stimulus: 0.8, chatTitle: "Hive inspection scheduler", suggested: "ag-scout", suggestedName: "scout", nearestThreshold: 0.5 },
  { id: "b2", kind: "unreviewedProposal", title: "Cold-day floor for inspections", sessionId: "chat-1", proposalId: "p1", sinceMs: now - 1_800_000, stimulus: 0.5, claimedBy: "Theo", chatTitle: "Hive inspection scheduler", suggested: null, suggestedName: null, nearestThreshold: null },
];

export const home = {
  beacon: { relayUrl: "https://relay.example.org", baseBranch: "main", coordinatorHostId: "h-box", note: "serve box coordinates runs", updatedBy: "Zaz", atMs: now - 86_400_000 },
  localRelayUrl: "https://relay.example.org",
  localBaseBranch: "main",
  localHostId: "h-mac",
  coordinatorLabel: "serve box",
  coordinatorOnline: false,
  mismatches: [],
};

export const connection = { relayUrl: "https://relay.example.org", room: "apiary", hasWorkspaceKey: true, hasApiKey: false, hasRelayAccessToken: true, permissionMode: "acceptEdits" };

export const providers = [
  { kind: "anthropic", name: "Anthropic", needsKey: true, hasKey: true, supportsBaseUrl: false, baseUrl: "", note: "" },
  { kind: "openai", name: "OpenAI", needsKey: true, hasKey: false, supportsBaseUrl: true, baseUrl: "", note: "" },
];

export const clis = [
  { id: "copilot", provider: "copilot", label: "GitHub Copilot CLI", binary: "copilot", installed: true, path: "/opt/homebrew/bin/copilot", version: "1.2.0", login: "loggedIn", loginDetail: "zazulam", loginCommand: "copilot login", installCommand: "", supported: true, note: "", models: ["gpt-5"] },
  { id: "antigravity", provider: "antigravity", label: "Antigravity", binary: "agy", installed: false, path: null, version: null, login: "unknown", loginDetail: "", loginCommand: "", installCommand: "brew install antigravity", supported: true, note: "", models: [] },
];

export const schedules = [{ id: "s1", enabled: true, label: "Morning stand-up digest", workspaceId: "ws-1", runtimeId: "rt-claude", prompt: "Summarize yesterday", trigger: { kind: "daily_at", hour: 8, minute: 30 }, lastRun: iso(300) }];

// ── Workspace repositories (Settings → Repositories, the repo suggestion) ──
export const workspaceRepos = {
  canAdmin: true,
  hostLabel: "MacBook Pro",
  coverageReporting: true,
  scanRoots: ["/Users/zaz/projects"],
  suggestion: null,
  hostAliases: [{ alias: "gh-work", host: "github.com", accepted: true }],
  repos: [
    {
      id: "repo-apiary",
      name: "apiary",
      canonical: "github.com/zaz/apiary",
      forgeKind: "github",
      defaultBranch: "main",
      removed: false,
      here: { state: "present", code: "present", viaFork: false, path: "/Users/zaz/projects/apiary", found: [] },
      hosts: [
        { hostId: "h-box", label: "serve box", kind: "worker", online: false, state: "present", viaFork: false },
        { hostId: "h-maya", label: "Maya's laptop", kind: "device", online: true, state: "present", viaFork: true },
      ],
      aliases: [{ alias: "github.com/apiary-org/apiary", accepted: true }],
    },
    {
      id: "repo-relay",
      name: "relay",
      canonical: "codeberg.org/maya/relay",
      forgeKind: "gitea",
      defaultBranch: "main",
      removed: false,
      here: { state: "missing", code: "no_checkout", viaFork: false, path: null, found: ["/Users/zaz/src/relay"] },
      hosts: [{ hostId: "h-box", label: "serve box", kind: "worker", online: false, state: "cloning", viaFork: false }],
      aliases: [{ alias: "codeberg.org/apiary/relay", accepted: false }],
    },
    {
      id: "repo-notes",
      name: "hive-notes",
      canonical: "local:9f2c41d07a",
      forgeKind: "local",
      defaultBranch: "main",
      removed: false,
      here: { state: "unknown", code: "unknown", viaFork: false, path: null, found: [] },
      hosts: [],
    },
  ],
};

// ── Workspace memory (the Memory pane) ──
const maya = { id: "a-maya", name: "Maya", kind: "human" as const };
const zaz = { id: "a-zaz", name: "Zaz", kind: "human" as const };
const scout = { id: "ag-scout", name: "scout", kind: "agent" as const };
const rev = (n: string, title: string, body: string, author: typeof zaz, minsAgo: number, tags: string[] = []) => ({
  rev: `rev-${n}`,
  title,
  body,
  tags,
  author,
  atMs: now - minsAgo * 60_000,
  source: { chatId: "chat-1" },
});
export const memoryView = (query: string) => {
  const entries = [
    {
      id: "mem-1",
      current: rev("1b", "Inspections need 15 °C or warmer", "Don't schedule hive inspections below 15 °C: opening the hive chills the brood. Use the forecast high, not the current reading.", maya, 40, ["scheduler", "weather"]),
      createdBy: maya,
      createdAtMs: now - 3 * 86_400_000,
      revisionCount: 2,
    },
    {
      id: "mem-2",
      current: rev("2a", "Relay sync is end-to-end encrypted", "Never log decrypted payloads in the relay client. The workspace key stays on member devices.", zaz, 600, ["security"]),
      createdBy: zaz,
      createdAtMs: now - 6 * 86_400_000,
      revisionCount: 1,
    },
    {
      id: "mem-3",
      current: rev("3a", "Varroa counts are per 300 bees", "Mite wash counts in the importer are per 300-bee sample; convert to a percentage before comparing hives.", scout, 1440, ["varroa"]),
      createdBy: scout,
      createdAtMs: now - 86_400_000,
      revisionCount: 1,
    },
  ];
  const q = query.trim().toLowerCase();
  const shown = q ? entries.filter((e) => `${e.current.title} ${e.current.body}`.toLowerCase().includes(q)) : entries;
  return {
    entries: shown,
    total: entries.length,
    proposals: [
      {
        proposal: {
          proposalId: "mp-1",
          op: { op: "create", entryId: "mem-4", title: "Run cargo test with --locked", body: "CI builds with --locked; run tests the same way so lockfile drift shows up locally.", tags: ["ci"] },
          proposer: "ag-scout",
          source: { chatId: "chat-2" },
          rationale: "Two turns failed in CI after passing locally.",
        },
        by: scout,
        atMs: now - 25 * 60_000,
      },
    ],
    chatTitles: { "chat-1": "Hive inspection scheduler", "chat-2": "Relay reconnect backoff" },
    agentNames: { "ag-scout": "scout", "ag-builder": "builder", primary: "ligus" },
    me: "a-zaz",
    moderator: true,
  };
};

// ── Workspace diff sides (the Diff tab's Monaco view), keyed by path ──
const diffSides: Record<string, { original: string; modified: string }> = {
  "src/scheduler.rs": {
    original: "pub fn next_due(&self, now: Date) -> Date {\n    self.queue.peek().copied().unwrap_or(now)\n}\n",
    modified:
      "pub fn next_due(&self, now: Date) -> Date {\n    let floor = self.cfg.min_temp_c;\n    self.queue\n        .iter()\n        .find(|s| s.forecast_high_c >= floor)\n        .copied()\n        .unwrap_or(now)\n}\n",
  },
  "src/weather.rs": {
    original: "pub struct Day {\n    pub date: NaiveDate,\n    pub rain: f32,\n}\n",
    modified: "pub struct Day {\n    pub date: NaiveDate,\n    pub rain: f32,\n    pub high_c: f32,\n}\n",
  },
  "tests/scheduler_cold.rs": {
    original: "",
    modified: "#[test]\nfn skips_cold_days() {}\n",
  },
};
export const fileDiffSides = (path: string) => ({ ...(diffSides[path] ?? { original: "", modified: "" }), isBinary: false });
