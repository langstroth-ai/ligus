import { useEffect, useState, type ReactNode } from "react";
import { InfoTip, fieldStyle } from "@/components/ui";
import { Fingerprint, groupMatchCode, spellMatchCode } from "@/components/kit/Fingerprint";
import {
  linkRequestCancel,
  linkRequestCodesMatch,
  linkRequestLinkAs,
  linkRequestOpenGrant,
  linkRequestPaste,
  linkRequestRedeem,
  linkRequestStart,
  linkRequestStatus,
  onLinkFlowChanged,
  type LinkFlowDto,
} from "@/lib/ipc";

const IDLE: LinkFlowDto = {
  phase: "idle",
  request: null,
  fingerprint: null,
  label: null,
  code: null,
  codeSpelled: null,
  claimedName: null,
  claimedFingerprint: null,
  personName: null,
  workspaceName: null,
  workspaceId: null,
  approverFingerprint: null,
  notice: null,
  error: null,
  canRetry: false,
};

/// The code both screens show, large, in its two groups (the Inbox's sheet
/// shows it the same way); spelled for screen readers.
function MatchCode({ code, dim = false }: { code: string; dim?: boolean }) {
  return (
    <p
      role="img"
      aria-label={`Code: ${spellMatchCode(code)}`}
      data-testid="match-code"
      className="flex justify-center gap-4 whitespace-nowrap py-2 text-[28px] font-semibold leading-9 tabular-nums"
      style={{ fontFamily: "var(--ligus-mono)", letterSpacing: "0.08em", color: dim ? "var(--ligus-ink-soft)" : "var(--ligus-ink)" }}
    >
      {groupMatchCode(code).map((g, i) => (
        <span key={i} aria-hidden>
          {g}
        </span>
      ))}
    </p>
  );
}

/// Phases with a request open: leaving ends it (nothing is granted).
const OPEN = new Set(["waiting", "code", "approving", "checking", "confirmPerson", "paste", "confirmApprover"]);

/// "I already use Ligus on another device": link this computer to the
/// person's existing account. It never asks for a name and never makes or
/// joins a workspace: this computer ends as one more device of the same
/// person, or nothing happens.
///
/// The person pastes the approval address their other device shows (My
/// devices → Add a computer). Both screens then show the same code; the
/// other device confirms with the passkey, and this computer confirms who it
/// links as. When the relay can't carry requests, this computer's request
/// code is pasted across instead, and the code the other device gives back
/// is pasted here.
export function LinkThisDevice({ onBack, onLinked }: { onBack: () => void; onLinked?: (flow: LinkFlowDto) => void | Promise<void> }) {
  const [flow, setFlow] = useState<LinkFlowDto>(IDLE);
  const [address, setAddress] = useState("");
  const [grant, setGrant] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | undefined;
    void onLinkFlowChanged((v) => {
      if (live) setFlow(v);
    })
      .then((u) => {
        if (live) unlisten = u;
        else u();
      })
      .catch(() => {});
    // A request still open from before this screen opened picks up where it
    // is; a finished one (linked, failed) isn't shown again.
    void linkRequestStatus()
      .then((v) => {
        if (live && v && (OPEN.has(v.phase) || v.phase === "linking")) setFlow(v);
      })
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  async function act(f: () => Promise<LinkFlowDto>) {
    setProblem(null);
    setBusy(true);
    try {
      setFlow(await f());
    } catch (e) {
      setProblem(String(e));
    } finally {
      setBusy(false);
    }
  }

  const start = () => act(() => linkRequestStart(address.trim()));
  const usePaste = () => act(linkRequestPaste);
  const cancel = () => act(linkRequestCancel);
  const back = () => {
    if (OPEN.has(flow.phase)) void linkRequestCancel().catch(() => {});
    onBack();
  };
  const finish = () =>
    act(async () => {
      await onLinked?.(flow);
      return flow;
    });

  async function copyRequest() {
    if (!flow.request) return;
    try {
      await navigator.clipboard.writeText(flow.request);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      setProblem(String(e));
    }
  }

  const primary = (label: string, onClick: () => void, disabled = false) => (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled || busy}
      className="w-full rounded-xl px-5 py-2.5 text-sm font-semibold disabled:opacity-40"
      style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
    >
      {busy ? "…" : label}
    </button>
  );
  const secondary = (label: string, onClick: () => void) => (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      className="w-full rounded-xl border px-5 py-2.5 text-sm disabled:opacity-40"
      style={{ borderColor: "var(--ligus-line)" }}
    >
      {label}
    </button>
  );
  const quiet = (label: string, onClick: () => void) => (
    <button type="button" onClick={onClick} disabled={busy} className="text-xs text-ink-soft hover:text-ink disabled:opacity-40">
      {label}
    </button>
  );
  const soft = (children: ReactNode, testId?: string) => (
    <p className="text-xs text-ink-soft" data-testid={testId}>
      {children}
    </p>
  );
  const field = "w-full rounded-xl border px-3 py-2 font-mono text-xs";

  let body: ReactNode;
  switch (flow.phase) {
    case "waiting":
      body = (
        <>
          {soft("Waiting for your other device. Open the request in its Inbox.", "link-waiting")}
          {flow.notice && soft(flow.notice)}
          {quiet("Cancel", cancel)}
        </>
      );
      break;
    case "code":
      body = (
        <>
          <p className="text-sm">Does your other device show this code?</p>
          {flow.code && <MatchCode code={flow.code} />}
          {flow.claimedName && soft(`Says it's ${flow.claimedName}'s device (checked after approval).`)}
          {primary("Codes match", () => void act(() => linkRequestCodesMatch(true)))}
          {secondary("They don't match", () => void act(() => linkRequestCodesMatch(false)))}
        </>
      );
      break;
    case "approving":
      body = (
        <>
          {flow.code && <MatchCode code={flow.code} dim />}
          {soft("Now confirm on your other device with your passkey.", "link-approving")}
          {flow.notice && soft(flow.notice)}
          {quiet("Cancel", cancel)}
        </>
      );
      break;
    case "checking":
      body = soft(`Checking the approval${flow.workspaceName ? ` on ${flow.workspaceName}` : ""}…`, "link-checking");
      break;
    case "confirmPerson":
      body = (
        <>
          <p className="text-sm">
            Link this computer as <strong>{flow.personName}</strong>?
          </p>
          {soft("Checked on the workspace's log. You stay one person there, on one more device.")}
          {primary("Link", () => void act(() => linkRequestLinkAs(true)))}
          {secondary("Cancel", () => void act(() => linkRequestLinkAs(false)))}
        </>
      );
      break;
    case "linking":
      body = soft("Linking…");
      break;
    case "linked":
      body = (
        <>
          <p className="text-sm" data-testid="link-linked">
            This computer is now one of {flow.personName ? `${flow.personName}'s` : "your"} devices.
          </p>
          {primary("Continue", () => void finish())}
        </>
      );
      break;
    case "paste":
      body = (
        <>
          {soft(
            "On your other device: My devices → Add a computer. Paste this request there, check the fingerprint matches, and confirm with your passkey.",
          )}
          <div className="flex items-center gap-2">
            <code
              className="min-w-0 flex-1 truncate rounded-lg border px-2 py-1 text-[11.5px]"
              style={{ borderColor: "var(--ligus-line)", fontFamily: "var(--ligus-mono)" }}
              data-testid="link-request-code"
            >
              {flow.request ?? "…"}
            </code>
            <button
              type="button"
              className="rounded-lg border px-2 py-1 text-xs"
              style={{ borderColor: "var(--ligus-line)" }}
              onClick={() => void copyRequest()}
            >
              {copied ? "Copied" : "Copy"}
            </button>
          </div>
          {flow.fingerprint && <Fingerprint value={flow.fingerprint} label="This computer's fingerprint" copyable={false} />}
          {soft("Then paste the code it gives back:")}
          <textarea
            value={grant}
            onChange={(e) => setGrant(e.target.value)}
            placeholder="ligusgrant1:…"
            rows={3}
            aria-label="The code from your other device"
            className={field}
            style={fieldStyle}
          />
          {primary("Continue", () => void act(() => linkRequestOpenGrant(grant)), grant.trim().length === 0)}
          {quiet("Cancel", cancel)}
        </>
      );
      break;
    case "confirmApprover":
      body = (
        <>
          <p className="text-sm">
            Approved by <strong>{flow.personName}</strong>'s device for {flow.workspaceName}. Is this your device's fingerprint?
          </p>
          {flow.approverFingerprint && <Fingerprint value={flow.approverFingerprint} label="The approving device's fingerprint" copyable={false} />}
          {soft("Your other device shows its own under Settings → Profile → My devices.")}
          {primary("Yes, link", () => void act(linkRequestRedeem))}
          {secondary("No", () => void cancel())}
        </>
      );
      break;
    case "noMailbox":
      body = (
        <>
          {soft(flow.error, "link-error")}
          {primary("Use a request code", () => void usePaste())}
        </>
      );
      break;
    case "failed":
      body = (
        <>
          <p className="text-xs" style={{ color: "var(--ligus-danger-ink)" }} data-testid="link-error">
            {flow.error}
          </p>
          {flow.canRetry && primary("Try again", () => void (address.trim() ? start() : cancel()))}
          {quiet("Start over", () => void cancel())}
        </>
      );
      break;
    default:
      body = (
        <>
          {soft(
            "On your other device, open Settings → Profile → My devices → Add a computer, and copy the approval address. Paste it here.",
          )}
          <textarea
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="ligusaddr1:…"
            rows={2}
            aria-label="The approval address from your other device"
            className={field}
            style={fieldStyle}
          />
          {primary("Continue", () => void start(), address.trim().length === 0)}
          <div className="flex items-center gap-1.5">
            {quiet("Use a request code instead", () => void usePaste())}
            <InfoTip
              label="request codes"
              text="When your other device can't get the request (a relay without approval mailboxes), copy this computer's request code to it instead, then paste the code it gives back."
            />
          </div>
        </>
      );
  }

  return (
    <div className="space-y-3" data-testid="link-this-device" data-phase={flow.phase}>
      <div className="flex items-center gap-1.5">
        <div className="text-sm font-medium">Link this computer</div>
        <InfoTip
          label="linking this computer"
          text="This computer becomes one more of your devices: you, with your name and your workspaces. Starting fresh here instead would make a second you in your workspaces."
        />
      </div>
      {body}
      {problem && (
        <p className="text-xs" style={{ color: "var(--ligus-danger-ink)" }} role="alert">
          {problem}
        </p>
      )}
      {flow.phase !== "linked" && flow.phase !== "linking" && (
        <button type="button" onClick={back} disabled={busy} className="text-xs text-ink-soft hover:text-ink disabled:opacity-40">
          Back
        </button>
      )}
    </div>
  );
}
