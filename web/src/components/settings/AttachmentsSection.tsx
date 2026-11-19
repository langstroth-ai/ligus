import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import {
  attachmentRules,
  blockAttachmentHash,
  setAttachmentPolicy,
  unblockAttachmentHash,
  type AttachmentPolicy,
} from "@/lib/ipc";

const ROLES: AttachmentPolicy["minRole"][] = ["viewer", "contributor", "admin", "owner"];
const SHA256 = /^[0-9a-f]{64}$/;

/// The workspace attachment policy (issue #118): part of the workspace rules,
/// changed only by an Admin with a passkey confirmation. Off by default.
/// Agents and service members stay off unless turned on here; even then an
/// agent's file is a proposal a person approves. Also the Admin blocklist of
/// file hashes.
export function AttachmentsSection() {
  const qc = useQueryClient();
  const rules = useQuery({ queryKey: ["attachment-rules"], queryFn: attachmentRules });
  const [draft, setDraft] = useState<AttachmentPolicy | null>(null);
  const [hash, setHash] = useState("");
  useEffect(() => {
    if (rules.data) setDraft(rules.data.policy);
  }, [rules.data]);
  const refresh = () => qc.invalidateQueries({ queryKey: ["attachment-rules"] });
  const save = useMutation({
    mutationFn: (p: AttachmentPolicy) => setAttachmentPolicy(p),
    onSuccess: () => {
      refresh();
      toast.success("Attachment policy saved.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const block = useMutation({
    mutationFn: (h: string) => blockAttachmentHash(h),
    onSuccess: () => {
      setHash("");
      refresh();
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const unblock = useMutation({ mutationFn: unblockAttachmentHash, onSuccess: refresh, onError: (e) => toast.error(errMsg(e)) });

  const r = rules.data;
  if (rules.isError) {
    return (
      <Section title="File attachments">
        <p role="alert" className="text-sm" style={{ color: "var(--ligus-danger-ink)" }}>
          Couldn't load the attachment rules: {errMsg(rules.error)}
        </p>
      </Section>
    );
  }
  if (!r || !draft) return <Section title="File attachments">Loading…</Section>;
  if (r.localOnly) {
    return (
      <Section title="File attachments">
        <p className="text-sm opacity-80">Files you attach in your own workspace stay on this device.</p>
      </Section>
    );
  }
  const edit = r.canEdit;
  const types = (list?: string[]) => (list ?? []).join(", ");
  const parseTypes = (s: string) =>
    s
      .split(/[\s,]+/)
      .map((t) => t.trim().toLowerCase())
      .filter(Boolean);
  return (
    <>
      <Section title="File attachments">
        <p className="mb-3 text-sm opacity-80">
          Shared files are end-to-end encrypted, so the relay can't scan them: every check happens on members' devices.
          Nothing opens automatically; risky files need a second confirmation and are marked as downloaded.
          {edit ? " Changes need your passkey." : " Only an Admin can change these."}
        </p>
        {!r.relay && <p className="mb-2 text-sm">This workspace has no relay, so files can't be shared yet.</p>}
        <fieldset disabled={!edit || save.isPending} className="flex flex-col gap-2 text-sm">
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} />
            Let members share files
          </label>
          <label className="flex items-center gap-2">
            Lowest role that may share
            <select value={draft.minRole} onChange={(e) => setDraft({ ...draft, minRole: e.target.value as AttachmentPolicy["minRole"] })} className="rounded border px-1" style={fieldStyle}>
              {ROLES.map((role) => (
                <option key={role} value={role}>
                  {role}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={draft.agents} onChange={(e) => setDraft({ ...draft, agents: e.target.checked })} />
            Let agents offer files (each one is a proposal a person approves)
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={draft.services} onChange={(e) => setDraft({ ...draft, services: e.target.checked })} />
            Let service members (serve boxes) share files
          </label>
          <label className="flex items-center gap-2">
            Largest file (MB)
            <input
              type="number"
              min={1}
              max={Math.floor(r.hardMaxBytes / (1024 * 1024))}
              value={Math.round(draft.maxBytes / (1024 * 1024))}
              onChange={(e) => setDraft({ ...draft, maxBytes: Math.max(1, Number(e.target.value) || 1) * 1024 * 1024 })}
              className="w-20 rounded border px-1"
              style={fieldStyle}
            />
          </label>
          <label className="flex flex-col gap-1">
            Blocked types (extensions like exe, or classes like @executable, @script, @installer, @diskimage, @archive, @macro)
            <input
              aria-label="Blocked types"
              defaultValue={types(draft.blockedTypes)}
              onBlur={(e) => setDraft({ ...draft, blockedTypes: parseTypes(e.target.value) })}
              className="rounded border px-1"
              style={fieldStyle}
            />
          </label>
          <label className="flex flex-col gap-1">
            Only these types (empty = any type that isn't blocked)
            <input
              aria-label="Allowed types"
              defaultValue={types(draft.allowedTypes)}
              onBlur={(e) => setDraft({ ...draft, allowedTypes: parseTypes(e.target.value) })}
              className="rounded border px-1"
              style={fieldStyle}
            />
          </label>
          {edit && (
            <div className="flex gap-2">
              <Button variant="primary" onClick={() => save.mutate(draft)}>
                Save policy
              </Button>
              {!r.policy.enabled && (
                <Button onClick={() => setDraft(r.recommended)}>Use the recommended policy</Button>
              )}
            </div>
          )}
        </fieldset>
      </Section>
      <Section title="Blocked files">
        <p className="mb-2 text-sm opacity-80">Files with these SHA-256 hashes are refused when shared and when downloaded, wherever they were shared.</p>
        <ul className="mb-2 flex flex-col gap-1 font-mono text-xs">
          {r.blocked.length === 0 && <li className="opacity-60">None</li>}
          {r.blocked.map((h) => (
            <li key={h} className="flex items-center gap-2">
              <span className="truncate">{h}</span>
              {edit && (
                <Button size="sm" onClick={() => unblock.mutate(h)}>
                  Unblock
                </Button>
              )}
            </li>
          ))}
        </ul>
        {edit && (
          <div className="flex gap-2">
            <input
              aria-label="SHA-256 to block"
              placeholder="sha256 of the file"
              value={hash}
              onChange={(e) => setHash(e.target.value.trim().toLowerCase())}
              className="flex-1 rounded border px-1 font-mono text-xs"
              style={fieldStyle}
            />
            <Button disabled={!SHA256.test(hash)} onClick={() => block.mutate(hash)}>
              Block
            </Button>
          </div>
        )}
      </Section>
    </>
  );
}
