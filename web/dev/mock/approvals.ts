// Dev-only: code-match approval requests with demo data (no real keys or
// addresses), for previews and screenshots. Knobs (URL query):
//   &approvals=box|computer|contested|several|mismatch
//   &approvalphase=confirming|passkey   (the session's phase once opened)

type Args = Record<string, unknown> | undefined;

const params = new URLSearchParams(window.location.search);
const scenario = params.get("approvals");
const forcedPhase = params.get("approvalphase");
const WS = "ws-1";
const t0 = Date.now();

function req(rid: string, kind: "box" | "computer" | "phone", label: string, agoMs: number, claim: unknown = { state: "unclaimed" }, parallel = 1) {
  return {
    workspaceId: WS,
    workspaceName: "Apiary",
    rid,
    kind,
    label,
    fingerprint: "7F3A-91C0-77DE-0B14-5E62",
    createdAt: t0 - agoMs,
    expiresAt: t0 - agoMs + 15 * 60_000,
    relay: "https://relay.example",
    claim,
    parallel,
    phase: null as string | null,
  };
}

function requests() {
  switch (scenario) {
    case "box":
      return [req("r-box", "box", "build-1", 120_000)];
    case "computer":
      return [req("r-lt", "computer", "lt on mbp", 40_000)];
    case "contested":
      return [req("r-box", "box", "build-1", 60_000, { state: "contested", name: null })];
    case "several":
      return ["build-1", "build-2", "build-3", "build-4", "build-5"].map((l, i) => req(`r-${i}`, "box", l, 60_000 * (i + 1), { state: "unclaimed" }, 5));
    default:
      return [];
  }
}

function facts() {
  if (scenario !== "mismatch") return [];
  return [{ key: "mismatch:r-old", kind: "mismatch", workspaceId: WS, label: "build-1", at: t0 - 25 * 60_000 }];
}

const sessions = new Map<string, Record<string, unknown>>();

function session(rid: string) {
  const r = requests().find((x) => x.rid === rid);
  if (!r) return null;
  return {
    rid,
    workspaceId: WS,
    kind: r.kind,
    label: r.label,
    fingerprint: r.fingerprint,
    relay: r.relay,
    createdAt: r.createdAt,
    phase: forcedPhase ?? "code",
    code: "K7Q4 M9X2",
    spelled: "K, 7, Q, 4. M, 9, X, 2.",
    expiresAt: Date.now() + 4 * 60_000 + 32_000,
    codeAt: Date.now() - 4_000,
    message: null,
  };
}

// A demo address: the shape of a real one, made of placeholder values.
const demoAddress =
  "ligusaddr1:" +
  btoa(
    JSON.stringify({ v: 1, relay: "https://relay.example", workspaceId: "00000000-0000-4000-8000-000000000001", founderPin: "0".repeat(32), epoch: 1, mailbox: "d".repeat(32), sealKey: "DEMO", putCap: "DEMO" }),
  )
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");

export function approvalsMock(cmd: string, args: Args): { handled: boolean; value?: unknown } {
  const rid = String(args?.rid ?? "");
  switch (cmd) {
    case "list_approvals":
      return { handled: true, value: { requests: requests().map((r) => ({ ...r, phase: (sessions.get(r.rid)?.phase as string) ?? null })), facts: facts() } };
    case "get_approval_address":
      return { handled: true, value: { address: demoAddress, relay: "https://relay.example", canReset: true } };
    case "reset_approval_address":
      return { handled: true, value: { address: demoAddress, relay: "https://relay.example", canReset: true } };
    case "approval_match_paste":
      return { handled: true, value: null };
    case "approval_session":
      return { handled: true, value: sessions.get(rid) ?? null };
    case "approval_open": {
      const s = session(rid);
      if (s) sessions.set(rid, s);
      return { handled: true, value: s };
    }
    case "approval_codes_match":
    case "approval_dont_match":
    case "approval_expire": {
      const s = sessions.get(rid);
      const phase = cmd === "approval_codes_match" ? (s?.kind === "computer" ? "confirming" : "approved") : cmd === "approval_dont_match" ? "mismatch" : "expired";
      const next = { ...(s ?? {}), phase };
      sessions.set(rid, next);
      return { handled: true, value: next };
    }
    case "approval_end":
    case "approval_decline":
    case "approval_forget":
    case "approval_dismiss_fact":
      sessions.delete(rid);
      return { handled: true, value: null };
    default:
      return { handled: false };
  }
}
