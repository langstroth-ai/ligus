// My devices → Add a computer: make another computer of yours (lt, the
// `ligus` CLI) one of your devices, instead of a second member with your
// name. Paste its link request, compare the fingerprint, confirm with your
// passkey, then hand the grant back to the computer: Copy for lt copies the
// grant alone (lt and `ligus devices redeem` read it from the clipboard), and
// Copy command copies a command that holds no grant. See
// docs/design/link-a-computer.md.

import { useState, type ReactNode } from "react";
import {
  addComputer,
  approvalMatchPaste,
  inspectComputerRequest,
  removeDuplicateMember,
  type ComputerGrantDto,
  type ComputerRequestDto,
} from "@/lib/ipc";
import { Button, fieldStyle } from "@/components/ui";
import { Fingerprint } from "@/components/kit/Fingerprint";
import { confirmDialog } from "@/components/Dialog";
import { toast, errMsg } from "@/components/Toast";
import { ApprovalAddress } from "@/components/ApprovalAddress";
import { shownWorkspace } from "@/lib/shownWorkspace";
import { approvalSheet } from "@/state/approvals";

type MemberRef = { memberId: string; displayName: string };

export function AddComputerPanel({
  onDone,
  onCancel,
  suggested,
}: {
  onDone: () => void;
  onCancel: () => void;
  /// Opened from People on a member that looks like you on another computer:
  /// offer to remove that member once the computer is added. The computer's
  /// own proof (`previousMember`) wins when it names someone.
  suggested?: MemberRef;
}) {
  const [code, setCode] = useState("");
  const [preview, setPreview] = useState<ComputerRequestDto | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [matches, setMatches] = useState(false);
  const [busy, setBusy] = useState(false);
  const [grant, setGrant] = useState<ComputerGrantDto | null>(null);
  const [removed, setRemoved] = useState(false);

  async function check(next: string) {
    setCode(next);
    setPreview(null);
    setProblem(null);
    setMatches(false);
    if (!next.trim()) return;
    try {
      // A request that is live in the mailbox: compare the code instead.
      const live = await approvalMatchPaste(next.trim()).catch(() => null);
      if (live) {
        approvalSheet.open(live.workspaceId, live.rid);
        onCancel();
        return;
      }
      setPreview(await inspectComputerRequest(next.trim()));
    } catch (e) {
      if (next.trim().startsWith("liguslink1:")) setProblem(errMsg(e));
      else setProblem("That isn't a computer's link request. On the computer, choose Link to my devices in lt, or run ligus devices request.");
    }
  }

  async function add() {
    if (!preview) return;
    setBusy(true);
    try {
      setGrant(await addComputer(code.trim(), preview.fingerprint));
    } catch (e) {
      toast.error(`Couldn't add the computer: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function removeOld(m: MemberRef, proven: boolean) {
    const ok = await confirmDialog(
      `Remove the member "${m.displayName}" from this workspace? Their past messages keep their name; nothing new from them is accepted.` +
        (proven ? "" : " The computer didn't prove it was them: remove them only if that member was you on it."),
      { danger: true },
    );
    if (!ok) return;
    try {
      await removeDuplicateMember(m.memberId);
      setRemoved(true);
      toast.success(`"${m.displayName}" was removed.`);
    } catch (e) {
      toast.error(`Couldn't remove them: ${errMsg(e)}`);
    }
  }

  // Who to offer to remove: the member the computer proved it was, else the
  // one People suggested.
  const proven = grant?.previousMember ?? preview?.previousMember ?? null;
  const toRemove = proven ?? suggested ?? null;

  if (grant) {
    return (
      <AddComputerGrant
        label={preview?.label || "The computer"}
        grant={grant}
        removal={
          toRemove && !removed ? (
            <div
              className="flex items-center justify-between gap-3 rounded-lg border px-2.5 py-2"
              style={fieldStyle}
              data-testid="add-computer-remove-old"
            >
              <span>
                {proven ? "This computer was" : "You opened this from"} the separate member{" "}
                <strong>"{toRemove.displayName}"</strong>.
              </span>
              <Button size="sm" onClick={() => void removeOld(toRemove, Boolean(proven))}>
                Remove…
              </Button>
            </div>
          ) : null
        }
        onDone={onDone}
      />
    );
  }

  const canAdd = preview && !preview.alreadyMine && !preview.revoked && !preview.someoneElses;
  return (
    <div className="space-y-2" data-testid="add-computer">
      {suggested && !preview && (
        <p className="text-xs" data-testid="add-computer-suggested">
          Linking <strong>"{suggested.displayName}"</strong>: you can remove that member once it's added.
        </p>
      )}
      {shownWorkspace() && <ApprovalAddress workspaceId={shownWorkspace()} hint={<>On the computer, run <code>ligus devices request --to</code> with it. The request comes to your Inbox.</>} />}
      <p className="text-xs text-ink-soft">
        Or paste: on the other computer, choose <em>Link to my devices</em> in lt (Settings or People, or the palette), or run{" "}
        <code>ligus devices request</code>. Paste what it shows. An invite would make a second member; this makes the
        computer yours.
      </p>
      <textarea
        value={code}
        onChange={(e) => void check(e.target.value)}
        placeholder="liguslink1:…"
        rows={3}
        className="w-full rounded-xl border px-3 py-2 font-mono text-xs"
        style={fieldStyle}
        aria-label="The computer's link request"
      />
      {problem && (
        <div className="text-xs" style={{ color: "var(--ligus-danger-ink)" }} role="alert">
          {problem}
        </div>
      )}
      {preview && (
        <div className="space-y-2 text-xs" data-testid="add-computer-request">
          <div className="text-sm">
            <strong>{preview.label || "An unnamed computer"}</strong> wants to act as you here
          </div>
          {preview.previousMember && (
            <div data-testid="add-computer-previous">
              This computer was the separate member <strong>"{preview.previousMember.displayName}"</strong> in this
              workspace. You can remove that member after adding it.
            </div>
          )}
          <div className="text-ink-soft">Compare all five groups with the computer's screen. Only add a computer you are using right now.</div>
          <Fingerprint value={preview.fingerprint} label="The computer's fingerprint" size="lg" copyable={false} />
          {preview.alreadyMine && <div className="text-ink-soft">Already one of your devices.</div>}
          {preview.revoked && (
            <div style={{ color: "var(--ligus-danger-ink)" }}>
              This computer's device was revoked here. On it, make a new request (it uses a new device key).
            </div>
          )}
          {preview.someoneElses && (
            <div style={{ color: "var(--ligus-danger-ink)" }}>
              This computer is already in the workspace as {preview.someoneElses}. On it, make a new request with a new
              device key (<code>ligus devices request --new-device</code>).
            </div>
          )}
          {canAdd && (preview.alsoCertifiedBy?.length ?? 0) > 0 && (
            <div className="text-ink-soft" data-testid="also-certified">
              {preview.alsoCertifiedBy!.join(", ")} also certified this computer's key. Add it only if it's the computer in front of you.
            </div>
          )}
          {canAdd && (
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={matches} onChange={(e) => setMatches(e.target.checked)} />
              The computer shows the same fingerprint
            </label>
          )}
          <p className="text-ink-soft">
            It acts as you without your account key: votes from it ask for your passkey, and devices, phones and keys stay
            on this desktop.
          </p>
        </div>
      )}
      <div className="flex gap-2">
        {canAdd && (
          <Button variant="primary" size="md" disabled={!matches || busy} onClick={() => void add()}>
            {busy ? "Waiting for your passkey…" : "Add with passkey"}
          </Button>
        )}
        <Button size="md" disabled={busy} onClick={onCancel}>
          {preview && canAdd ? "Not my computer" : "Cancel"}
        </Button>
      </div>
    </div>
  );
}

/// The shell command the sheet copies for a terminal: no grant in it (it
/// would land in the shell's history and in `ps`); lt reads the grant from
/// the clipboard or a hidden paste, and the flag pins this desktop so lt
/// needn't ask.
export function redeemCommand(thisDesktop: string): string {
  return `lt --plain link-redeem --approver-fingerprint ${thisDesktop}`;
}

/// After the passkey: hand the grant to the computer. One main action, Copy
/// for lt (the grant alone); the command and the code itself are secondary.
function AddComputerGrant({
  label,
  grant,
  removal,
  onDone,
}: {
  label: string;
  grant: ComputerGrantDto;
  removal: ReactNode;
  onDone: () => void;
}) {
  const [copied, setCopied] = useState<"grant" | "command" | null>(null);
  const [showCode, setShowCode] = useState(false);
  const copy = (what: "grant" | "command", text: string) => {
    void (navigator.clipboard?.writeText(text) ?? Promise.reject(new Error("no clipboard")))
      .then(() => {
        setCopied(what);
        setTimeout(() => setCopied((c) => (c === what ? null : c)), 2000);
      })
      .catch(() => toast.error("Couldn't copy: select the code below and copy it."));
  };
  return (
    <div className="space-y-2.5 text-xs" data-testid="add-computer-grant">
      <p className="text-sm">
        <strong>{label}</strong> is one of your devices. Give it this code within {grant.expiresInMinutes} minutes.
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="primary" size="md" onClick={() => copy("grant", grant.grant)}>
          {copied === "grant" ? "Copied" : "Copy for lt"}
        </Button>
        <Button size="sm" onClick={() => copy("command", redeemCommand(grant.thisDesktop))}>
          {copied === "command" ? "Copied" : "Copy command"}
        </Button>
        <button
          type="button"
          className="text-ink-soft underline-offset-2 hover:underline"
          aria-expanded={showCode}
          onClick={() => setShowCode((v) => !v)}
        >
          {showCode ? "Hide code" : "Show code"}
        </button>
      </div>
      <p className="text-ink-soft" data-testid="add-computer-how">
        In lt: paste with ⌘V (Ctrl+Shift+V on Linux) in Add to my devices. In a shell: run{" "}
        <code>lt --plain link-redeem</code> (or <code>ligus devices redeem</code>) and press Enter to read the clipboard.
      </p>
      {showCode && (
        <textarea
          readOnly
          value={grant.grant}
          rows={3}
          className="w-full rounded-xl border px-3 py-2 font-mono text-[11px]"
          style={fieldStyle}
          aria-label="Grant for the computer"
          onFocus={(e) => e.currentTarget.select()}
        />
      )}
      <p className="text-ink-soft">It's sealed: only that computer can open it. It will ask you to check this desktop:</p>
      <Fingerprint value={grant.thisDesktop} label="This desktop's fingerprint" size="lg" copyText={grant.thisDesktop} />
      {removal}
      <Button size="md" onClick={onDone}>
        Done
      </Button>
    </div>
  );
}
