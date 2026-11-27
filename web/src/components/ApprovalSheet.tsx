// The code-match approval sheet (docs/design/code-match-approval.md 7.1,
// 7.2): one short code both screens show, compared by eye. The person
// confirms a match with their passkey; a mismatch grants nothing.
//
// The code is not a secret: it shows during screen sharing, and on purpose
// has no Copy button (it is for comparing, not for sending). Codes match
// enables 3 seconds after the code appears.
//
// A passkey hand-off (kind `presence`) uses the same sheet: another device
// of yours (the CLI, lt) sent one drafted change. The sheet shows what it
// does (rendered from the draft by this desktop, never from the other
// device's words) and the code that device printed; Confirm with passkey
// signs exactly that change, Not me writes nothing.

import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  approvalCodesMatch,
  approvalDontMatch,
  approvalEnd,
  approvalExpire,
  approvalForget,
  approvalOpen,
  approvalSession,
  type ApprovalDto,
  type ApprovalMoreDto,
  type ApprovalSessionDto,
} from "@/lib/ipc";
import { Countdown, Sheet, groupMatchCode, spellMatchCode } from "@/components/kit";
import { Button, InfoTip } from "@/components/ui";
import { errMsg, toast } from "@/components/Toast";
import { APPROVALS_KEY, sessionKey, useApprovals } from "@/state/approvals";
import { quoteLabel } from "@/lib/needsYou";

/// How long Codes match stays disabled after the code appears.
export const HOLD_MS = 3_000;

export const COPY = {
  aboutMatch:
    "The same code shows on both screens only when this desktop and the device that asked are talking to each other directly. A different code means something may be in between: choose Don't match.",
  checkBox: "Check the box shows the same code.",
  checkMine: "Check the device you're holding shows the same code.",
  contested: "Opened by a device that can't approve this. It may be an attempt to intercept it.",
  waitingComputer: "Waiting for the computer to confirm…",
  aboutMismatch:
    "The two screens showed different codes, so someone may have tried to intercept the request. Check the device you're adding and start again; tell an Admin if it happens twice.",
  aboutHandoff:
    "A terminal of yours has no passkey, so it sent this change here. Your passkey signs exactly the change shown, nothing else, and only for this request. The terminal shows the same code. If you didn't ask for this, choose Not me: nothing is written.",
  checkTerminal: "Check the terminal that asked shows the same code.",
};

/// The sheet's title, from the request's kind only: the requester's label
/// never leads (it sits quoted on its own line beneath).
export function titleFor(kind: string): string {
  if (kind === "presence") return "Confirm with your passkey?";
  return kind === "box" ? "Admit a box?" : kind === "phone" ? "Add a phone to your devices?" : "Add a computer to your devices?";
}

/// The small kind word beside the label, from the request's kind.
export function kindWord(kind: string): string {
  return kind === "box" ? "Box" : kind === "phone" ? "Phone" : kind === "presence" ? "Asked by" : "Computer";
}

/// The quoted label on its own line, after the kind word.
function Subject({ kind, label }: { kind: string; label: string }) {
  return (
    <span className="flex min-w-0 items-baseline gap-2" data-testid="approval-subject">
      <span className="shrink-0 text-[11.5px] font-medium uppercase tracking-wide">{kindWord(kind)}</span>
      <span className="min-w-0 break-words" style={{ color: "var(--ligus-ink)" }}>
        {quoteLabel(label)}
      </span>
    </span>
  );
}

/// The code, large, one line, two groups; spelled for screen readers.
export function MatchCode({ code, dim = false }: { code: string; dim?: boolean }) {
  const groups = groupMatchCode(code);
  return (
    <p
      role="img"
      aria-label={`Code: ${spellMatchCode(code)}`}
      data-testid="match-code"
      className="flex justify-center gap-4 whitespace-nowrap py-2 text-[28px] font-semibold leading-9 tabular-nums"
      style={{ fontFamily: "var(--ligus-mono)", letterSpacing: "0.08em", color: dim ? "var(--ligus-ink-soft)" : "var(--ligus-ink)" }}
    >
      {groups.map((g, i) => (
        <span key={i} aria-hidden className="whitespace-nowrap">
          {g}
        </span>
      ))}
    </p>
  );
}

function when(ms: number): string {
  try {
    return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  } catch {
    return "";
  }
}

function Details({
  kind,
  label,
  fingerprint,
  createdAt,
  relay,
  nameClash,
  changeKind,
}: {
  kind: string;
  label: string;
  fingerprint: string;
  createdAt: number;
  relay: string;
  nameClash?: boolean;
  changeKind?: string | null;
}) {
  const row = (k: string, v: string, mono = false) => (
    <div className="flex gap-3">
      <dt className="w-24 shrink-0 text-ink-soft">{k}</dt>
      <dd className={`min-w-0 break-all ${mono ? "font-mono" : ""}`}>{v}</dd>
    </div>
  );
  return (
    <details className="mt-4 text-[12.5px]" data-testid="approval-details">
      <summary className="cursor-pointer select-none text-ink-soft">Details</summary>
      <dl className="mt-2 space-y-1">
        {row(
          "Kind",
          kind === "box"
            ? "A box joining as its own member"
            : kind === "computer"
              ? "A computer of yours"
              : kind === "presence"
                ? "A change another device of yours sent for your passkey"
                : "A phone of yours",
        )}
        {row(kind === "presence" ? "Device" : "Label", quoteLabel(label))}
        {kind === "presence" && changeKind && row("Change", changeKind, true)}
        {row("Fingerprint", fingerprint, true)}
        {row("Asked at", when(createdAt))}
        {row("Relay", relay, true)}
      </dl>
      {nameClash && (
        <p className="mt-2 text-ink-soft" data-testid="approval-name-clash">
          This name matches an existing device.
        </p>
      )}
    </details>
  );
}

const MORE_STATE: Record<string, string> = {
  offered: "Comes along",
  skipped: "Not added",
  leftOut: "Not added",
  notTaken: "Not added",
  adding: "Passkey…",
  added: "Added",
  notAdded: "Not added",
};

/// A computer link's other workspaces: one row each, with why one isn't
/// added (this desktop can't add it there, its passkey was cancelled…).
function MoreWorkspaces({ rows }: { rows?: ApprovalMoreDto[] }) {
  if (!rows || rows.length === 0) return null;
  return (
    <div className="space-y-1.5" data-testid="approval-more">
      <p className="text-[12px] text-ink-soft">Your other workspaces, one passkey each:</p>
      <ul className="space-y-1">
        {rows.map((m) => (
          <li key={m.workspaceId} className="text-[12px]" data-testid={`approval-more-${m.state}`}>
            <div className="flex items-center justify-between gap-2">
              <span className="min-w-0 truncate">{m.name}</span>
              <span className="shrink-0 text-ink-soft">{MORE_STATE[m.state] ?? m.state}</span>
            </div>
            {m.message && <p className="text-ink-soft">{m.message}</p>}
          </li>
        ))}
      </ul>
    </div>
  );
}

export function ApprovalSheet({ workspaceId, rid, onClose, now = Date.now }: { workspaceId: string; rid: string; onClose: () => void; now?: () => number }) {
  const qc = useQueryClient();
  const { requests, loaded } = useApprovals();
  const request: ApprovalDto | undefined = requests.find((r) => r.rid === rid);
  const claim = request?.claim.state ?? "unclaimed";
  // Someone else is checking it, or a device that can't approve opened it:
  // don't open it from here.
  const blocked = !request?.phase && (claim === "claimedBy" || claim === "contested");
  // Begun here, but a device that can't approve opened it too: still warn.
  const contestedWhileWaiting = request?.phase === "waiting" && claim === "contested";
  const [openError, setOpenError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const opened = useRef(false);

  const session = useQuery({
    queryKey: sessionKey(rid),
    queryFn: async () => (await approvalSession(rid)) ?? null,
    refetchInterval: 4_000,
  });
  const s: ApprovalSessionDto | null = session.data ?? null;

  useEffect(() => {
    // Read the claims first: a contested or claimed request isn't opened.
    if (opened.current || !loaded || blocked || session.isLoading || s) return;
    opened.current = true;
    approvalOpen(workspaceId, rid)
      .then((v) => qc.setQueryData(sessionKey(rid), v))
      .catch((e) => setOpenError(errMsg(e)));
  }, [loaded, blocked, session.isLoading, s, workspaceId, rid, qc]);

  // The hold: Codes match enables HOLD_MS after the code appears.
  const [tick, setTick] = useState(() => now());
  useEffect(() => {
    if (s?.phase !== "code") return;
    const t = setInterval(() => setTick(now()), 250);
    return () => clearInterval(t);
  }, [s?.phase, now]);
  const shownAt = s?.codeAt ?? 0;
  const held = s?.phase === "code" && tick - shownAt >= HOLD_MS;

  const kind = s?.kind ?? request?.kind ?? "box";
  const label = s?.label ?? request?.label ?? "";
  const mine = kind !== "box";
  const handoff = kind === "presence";
  const summary = s?.summary ?? request?.summary ?? "";
  const final = s ? !["waiting", "code", "confirming", "passkey"].includes(s.phase) : false;

  function close() {
    if (final) void approvalForget(rid).catch(() => {});
    void qc.invalidateQueries({ queryKey: APPROVALS_KEY });
    onClose();
  }

  async function run(f: () => Promise<unknown>) {
    if (busy) return;
    setBusy(true);
    try {
      const v = await f();
      if (v && typeof v === "object" && "rid" in v) qc.setQueryData(sessionKey(rid), v);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function endIt() {
    await run(() => approvalEnd(workspaceId, rid));
    close();
  }

  const corner =
    s?.expiresAt && (s.phase === "code" || s.phase === "waiting") ? (
      <Countdown
        expiresAt={s.expiresAt}
        ring={false}
        label={handoff ? "The request expires in" : "The code expires in"}
        onExpire={() => void run(() => approvalExpire(rid))}
        now={now}
      />
    ) : null;

  let body: React.ReactNode;
  let footer: React.ReactNode = null;
  if ((blocked && claim === "contested") || (contestedWhileWaiting && (!s || s.phase === "waiting"))) {
    body = (
      <p className="text-[13px]" role="alert" style={{ color: "var(--ligus-danger-ink)" }} data-testid="approval-contested">
        {COPY.contested}
      </p>
    );
    footer = (
      <>
        <Button size="md" onClick={close}>
          Close
        </Button>
        <Button variant="danger" size="md" disabled={busy} onClick={() => void endIt()}>
          End this request
        </Button>
      </>
    );
  } else if (blocked && request?.claim.state === "claimedBy") {
    body = (
      <p className="text-[13px]" data-testid="approval-claimed">
        {request.claim.name ? quoteLabel(request.claim.name) : "Another device"} is checking this request
      </p>
    );
    footer = (
      <Button size="md" onClick={close}>
        Close
      </Button>
    );
  } else if (openError && !s) {
    body = (
      <p className="text-[13px]" role="alert" style={{ color: "var(--ligus-danger-ink)" }}>
        {openError}
      </p>
    );
    footer = (
      <Button size="md" onClick={close}>
        Close
      </Button>
    );
  } else if (!s || s.phase === "waiting") {
    body = (
      <p className="py-4 text-center text-[13px] text-ink-soft" data-testid="approval-waiting">
        {kind === "box" ? "Waiting for the box to answer…" : "Waiting for the device to answer…"}
      </p>
    );
  } else if (handoff && s.phase === "code" && s.code) {
    body = (
      <>
        <p className="text-[12.5px] text-ink-soft">Asks to:</p>
        <p className="mt-1 rounded-md border px-3 py-2 text-[13.5px]" style={{ borderColor: "var(--ligus-line)", color: "var(--ligus-ink)" }} data-testid="handoff-summary">
          {summary}
        </p>
        <MatchCode code={s.code} />
        <p className="text-center text-[13px]">{COPY.checkTerminal}</p>
        {s.message && (
          <p className="mt-2 text-center text-[12.5px]" role="alert" style={{ color: "var(--ligus-danger-ink)" }}>
            {s.message}
          </p>
        )}
      </>
    );
    footer = (
      <>
        <Button size="md" disabled={busy} onClick={() => void run(() => approvalDontMatch(rid))}>
          Not me
        </Button>
        <Button variant="primary" size="md" disabled={!held || busy} onClick={() => void run(() => approvalCodesMatch(rid))}>
          Confirm with passkey
        </Button>
      </>
    );
  } else if (s.phase === "code" && s.code) {
    body = (
      <>
        <MatchCode code={s.code} />
        <p className="text-center text-[13px]">{mine ? COPY.checkMine : COPY.checkBox}</p>
        {(request?.parallel ?? 0) >= 2 && (
          <p className="mt-2 text-center text-[12.5px] text-ink-soft" data-testid="approval-parallel">
            {request!.parallel} devices are asking. Check the code on the one you're holding.
          </p>
        )}
        {s.message && (
          <p className="mt-2 text-center text-[12.5px]" role="alert" style={{ color: "var(--ligus-danger-ink)" }}>
            {s.message}
          </p>
        )}
      </>
    );
    footer = (
      <>
        <Button size="md" disabled={busy} onClick={() => void run(() => approvalDontMatch(rid))}>
          Don't match
        </Button>
        <Button variant="primary" size="md" disabled={!held || busy} onClick={() => void run(() => approvalCodesMatch(rid))}>
          Codes match
        </Button>
      </>
    );
  } else if (s.phase === "confirming") {
    body = (
      <>
        {s.code && <MatchCode code={s.code} dim />}
        <p className="text-center text-[13px] text-ink-soft" role="status" data-testid="approval-confirming">
          {COPY.waitingComputer}
        </p>
        <MoreWorkspaces rows={s.more} />
      </>
    );
  } else if (s.phase === "passkey") {
    body = (
      <>
        {s.code && <MatchCode code={s.code} dim />}
        <p className="text-center text-[13px] text-ink-soft" role="status" data-testid="approval-passkey">
          {s.step ? `Confirm with your passkey (${s.step})…` : "Confirm with your passkey…"}
        </p>
        <MoreWorkspaces rows={s.more} />
      </>
    );
  } else if (s.phase === "approved") {
    const added = (s.more ?? []).filter((m) => m.state === "added").length;
    body = (
      <>
        <p className="text-[13px]" role="status" data-testid="approval-approved">
          {kind === "box"
            ? `The box ${quoteLabel(label)} is admitted. It joins on its own.`
            : handoff
              ? `Confirmed. ${quoteLabel(label)} writes the change.`
              : added > 0
                ? `The computer ${quoteLabel(label)} is one of your devices in ${added + 1} workspaces.`
                : `The computer ${quoteLabel(label)} is one of your devices.`}
        </p>
        <MoreWorkspaces rows={s.more} />
      </>
    );
    footer = (
      <Button variant="primary" size="md" onClick={close}>
        Done
      </Button>
    );
  } else {
    const text = handoff
      ? s.phase === "declined"
        ? "Nothing was written. Your Inbox offers to revoke that device if you didn't ask for this."
        : (s.message ?? "Nothing was confirmed.")
      : s.phase === "mismatch"
        ? `Request from ${quoteLabel(label)} didn't match. Nothing was granted.`
        : s.phase === "didntFinish"
          ? `Linking ${quoteLabel(label)} didn't finish. Nothing was added.`
          : s.phase === "expired"
            ? "Expired. Nothing was granted."
            : s.phase === "ended" || s.phase === "declined"
              ? "The request was ended. Nothing was granted."
              : (s.message ?? "The request was ended. Nothing was granted.");
    body = (
      <p className="flex items-center gap-2 text-[13px]" role="status" data-testid="approval-over">
        <span>{text}</span>
        {s.phase === "mismatch" && <InfoTip label="a mismatch" text={COPY.aboutMismatch} />}
      </p>
    );
    footer = (
      <Button size="md" onClick={close}>
        Close
      </Button>
    );
  }

  return (
    <Sheet
      title={titleFor(kind)}
      description={<Subject kind={kind} label={label} />}
      titleAddon={handoff ? <InfoTip label="what this confirms" text={COPY.aboutHandoff} /> : <InfoTip label="what a match proves" text={COPY.aboutMatch} />}
      corner={corner}
      side="center"
      width={460}
      onClose={close}
      footer={footer}
    >
      <div data-testid="approval-sheet">
        {body}
        {(s || request) && (
          <Details
            kind={kind}
            label={label}
            fingerprint={s?.fingerprint ?? request?.fingerprint ?? ""}
            createdAt={s?.createdAt ?? request?.createdAt ?? 0}
            relay={s?.relay ?? request?.relay ?? ""}
            nameClash={request?.nameClash}
            changeKind={s?.changeKind}
          />
        )}
      </div>
    </Sheet>
  );
}

/// The sheet, wherever it was opened from (mounted once at the app root).
export function ApprovalSheetHost({ open, onClose }: { open: { workspaceId: string; rid: string } | null; onClose: () => void }) {
  if (!open) return null;
  return <ApprovalSheet key={open.rid} workspaceId={open.workspaceId} rid={open.rid} onClose={onClose} />;
}
