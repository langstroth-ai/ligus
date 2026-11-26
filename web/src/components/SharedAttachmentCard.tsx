import { useState } from "react";
import {
  checkAttachment,
  deleteAttachment,
  previewAttachment,
  saveReceivedAttachment,
  type AttachmentCheck,
  type AttachmentPreview,
  type SharedAttachmentView,
} from "@/lib/ipc";
import { formatBytes, previewSrcDoc } from "@/lib/attachments";
import { confirmDialog } from "@/components/Dialog";
import { toast, errMsg } from "@/components/Toast";
import { IconFile } from "@/lib/icons";

/// What the desktop says when the relay no longer has a file.
export const EXPIRED_ON_RELAY = "This file expired on the relay";

/// A file shared over the relay (issue #118). Nothing is fetched, opened or
/// previewed until the person asks: "Check" downloads and verifies it against
/// its signed reference and inspects it; "Save…" writes it (quarantine-marked)
/// where they choose, with a second confirmation naming the sender for risky
/// types; "Preview" exists only for inert formats and renders in a sandboxed
/// frame with no scripts and no network.
export function SharedAttachmentCard({ sessionId, view }: { sessionId: string; view: SharedAttachmentView }) {
  const [check, setCheck] = useState<AttachmentCheck | null>(null);
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<AttachmentPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expiredNow, setExpiredNow] = useState(false);

  if (view.deleted) {
    return (
      <span className="inline-flex items-center gap-1.5 rounded-lg border px-2 py-1 text-xs text-ink-soft" style={{ borderColor: "var(--ligus-line)" }}>
        <IconFile size={13} /> {view.name} (deleted)
      </span>
    );
  }

  // Gone from the relay (expired, or never renewed): a quiet note instead
  // of a download that can only fail.
  if (view.expired || expiredNow) {
    return (
      <span
        data-testid="shared-attachment-expired"
        className="inline-flex max-w-full items-center gap-1.5 rounded-lg border px-2 py-1 text-xs text-ink-soft"
        style={{ borderColor: "var(--ligus-line)" }}
      >
        <IconFile size={13} />
        <span className="min-w-0 truncate" data-tip={view.name}>
          {view.name}
        </span>
        <span>· {EXPIRED_ON_RELAY}</span>
      </span>
    );
  }

  async function runCheck() {
    setBusy(true);
    setError(null);
    try {
      setCheck(await checkAttachment(sessionId, view.id));
    } catch (e) {
      const msg = errMsg(e);
      if (msg.includes(EXPIRED_ON_RELAY)) setExpiredNow(true);
      else setError(msg);
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    if (!check) return;
    let confirmed = false;
    if (check.needsConfirmation) {
      const what = check.assessment.warnings.join(" ") || "This file can run code when opened.";
      const first = await confirmDialog(`${what}\n\nOnly open it if you expected it from ${check.sender}.`, {
        title: `Save a risky file from ${check.sender}?`,
        confirmLabel: "Continue",
        danger: true,
      });
      if (!first) return;
      confirmed = await confirmDialog(
        `“${check.name}” is from ${check.sender}. Your computer will mark it as downloaded, so it asks again before opening it. Save it?`,
        { title: "Confirm again", confirmLabel: `Save file from ${check.sender}`, danger: true },
      );
      if (!confirmed) return;
    }
    try {
      const path = await saveReceivedAttachment(sessionId, view.id, confirmed);
      if (path) toast.success(`Saved to ${path}`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function showPreview() {
    try {
      setPreview(await previewAttachment(sessionId, view.id));
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function remove() {
    const ok = await confirmDialog(`Delete “${view.name}” for everyone in this chat? Its encrypted copy is removed from the relay.`, {
      title: "Delete file",
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await deleteAttachment(sessionId, view.id);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  const risky = check ? check.assessment.risky : view.riskyByName;
  const refused = check?.refused ?? (view.blocked ? "This file is blocked in this workspace." : null);
  return (
    <div
      data-testid="shared-attachment"
      className="flex w-72 max-w-full flex-col gap-1 rounded-lg border px-2 py-1.5 text-xs"
      style={{ borderColor: risky ? "var(--ligus-danger, #c0392b)" : "var(--ligus-line)", background: "var(--ligus-mist)" }}
    >
      <div className="flex items-center gap-1.5">
        <span aria-hidden className="text-ink-soft">
          <IconFile size={13} />
        </span>
        <span className="min-w-0 flex-1 truncate font-medium" data-tip={view.name}>
          {view.name}
        </span>
        <span className="text-ink-soft">{formatBytes(view.size)}</span>
      </div>
      <div className="text-ink-soft">
        From {view.sender}
        {view.fromAgentOffer ? " (an agent's file, approved by a person)" : ""}
      </div>
      {risky && (
        <div role="alert" style={{ color: "var(--ligus-danger, #c0392b)" }}>
          {check ? check.assessment.warnings[0] ?? "This file can run code when opened." : "Its name says it can run code when opened."}
        </div>
      )}
      {check && (
        <div className="text-ink-soft" data-testid="attachment-check">
          Verified against what {check.sender} signed. Content: {check.assessment.detected.ext} ({check.assessment.detected.mime}).
          {check.archive && ` Archive: ${check.archive.entries} entries${check.archive.bomb ? `, ${check.archive.bomb}` : ""}.`}
          {check.archive && (check.archive.opaqueEntries ?? 0) > 0 &&
            ` ${check.archive.opaqueEntries} of its entries couldn't be opened (encrypted or in a format Ligus doesn't read), so their types weren't checked.`}
          {check.scan.verdict === "clean" && ` ${check.scan.scanner}: no threat found.`}
          {check.scan.verdict === "infected" && ` ${check.scan.scanner}: ${check.scan.detail}`}
          {check.scan.verdict === "failed" && ` ${check.scan.scanner} couldn't scan it (${check.scan.detail}).`}
        </div>
      )}
      {refused && <div role="alert">{refused}</div>}
      {error && <div role="alert">{error}</div>}
      <div className="flex flex-wrap gap-1.5">
        {!check && !view.blocked && (
          <button type="button" disabled={busy} onClick={() => void runCheck()} className="rounded border px-1.5 py-0.5" style={{ borderColor: "var(--ligus-line)" }}>
            {busy ? "Checking…" : "Check"}
          </button>
        )}
        {check && !check.refused && (
          <button type="button" onClick={() => void save()} className="rounded border px-1.5 py-0.5" style={{ borderColor: "var(--ligus-line)" }}>
            Save…
          </button>
        )}
        {check && !check.refused && check.assessment.preview && !preview && (
          <button type="button" onClick={() => void showPreview()} className="rounded border px-1.5 py-0.5" style={{ borderColor: "var(--ligus-line)" }}>
            Preview
          </button>
        )}
        {view.canDelete && (
          <button type="button" onClick={() => void remove()} className="rounded border px-1.5 py-0.5 text-ink-soft" style={{ borderColor: "var(--ligus-line)" }}>
            Delete
          </button>
        )}
      </div>
      {preview && (
        <iframe
          title={`Preview of ${view.name}`}
          data-testid="attachment-preview"
          // An empty sandbox: no scripts, no same-origin, no forms, no popups,
          // no navigation. The srcdoc's CSP also blocks every network load.
          sandbox=""
          referrerPolicy="no-referrer"
          srcDoc={previewSrcDoc(preview.kind, preview.mime, preview.dataBase64)}
          className="h-48 w-full rounded border"
          style={{ borderColor: "var(--ligus-line)", background: "white" }}
        />
      )}
    </div>
  );
}
