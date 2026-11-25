// Scratch repositories on the desktop: the "scratch" tag, the removal
// notice, "Keep it as a repo" (with an optional, explicitly confirmed
// "Publish to GitHub (private)"), and the compact part of Settings >
// Repositories. The backend is app/src/scratch_ui.rs.

import { useState } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import {
  createScratchRepo,
  forgesList,
  keepScratchRepo,
  listScratchRepos,
  pickWorkspaceFolder,
  publishScratchRepo,
  removeScratchRepo,
  setScratchSettings,
  type ScratchRepoDto,
  type ScratchReposDto,
} from "@/lib/ipc";
import { Badge, Button, EmptyHint, Field, ListRow, RowList, Section, SelectField, fieldStyle } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { confirmDialog } from "@/components/Dialog";
import { toast, errMsg } from "@/components/Toast";

export const SCRATCH_KEY = ["scratch-repos"];
/// Same as REPOS_KEY in settings/RepositoriesSection (kept literal here so
/// the two modules don't import each other).
const WORKSPACE_REPOS_KEY = ["workspace-repos"];

/// After any scratch change: the repo list, the scratch list and every
/// chat's repo chip.
export function refreshScratch(qc: QueryClient) {
  void qc.invalidateQueries({ queryKey: WORKSPACE_REPOS_KEY });
  void qc.invalidateQueries({ queryKey: SCRATCH_KEY });
  void qc.invalidateQueries({ queryKey: ["chat-repo"] });
}

/// A unix-seconds date in this locale ("Dec 9, 2026").
export function formatScratchDate(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleDateString(undefined, { dateStyle: "medium" });
}

/// The notice line's text (without the "Keep it" action).
export function scratchNoticeText(e: Pick<ScratchRepoDto, "removeAt">): string | null {
  return e.removeAt ? `This scratch repo will be removed on ${formatScratchDate(e.removeAt)}` : null;
}

/// The folder name without its scratch id ("spike-1a2b3c4d" → "spike").
export function scratchBaseName(folder: string): string {
  return folder.replace(/-[0-9a-f]{8}$/i, "") || folder;
}

/// The home folder a path lives in, when it plainly shows one.
function homeOf(path: string): string | null {
  const m = path.match(/^(\/Users\/[^/]+|\/home\/[^/]+|[A-Za-z]:\\Users\\[^\\]+)(?=[/\\]|$)/);
  return m ? m[1] : null;
}

/// Where "Keep it as a repo" suggests moving it: ~/projects/<name>.
export function suggestKeepPath(e: Pick<ScratchRepoDto, "folder" | "path">): string {
  const base = scratchBaseName(e.folder);
  const home = homeOf(e.path);
  if (!home) return `~/projects/${base}`;
  return home.includes("\\") ? `${home}\\projects\\${base}` : `${home}/projects/${base}`;
}

/// The small "scratch" tag.
export function ScratchTag() {
  return <Badge title="A throwaway repository on one device. Removed after a while without activity unless you keep it.">scratch</Badge>;
}

/// "This scratch repo will be removed on Dec 9, 2026 · Keep it".
export function ScratchNotice({ entry, onKeep }: { entry: ScratchRepoDto; onKeep: () => void }) {
  const text = entry.notice && entry.state === "live" ? scratchNoticeText(entry) : null;
  if (!text) return null;
  return (
    <span role="status" className="inline-flex flex-wrap items-center gap-1.5 text-xs" style={{ color: "var(--ligus-ink-soft)" }}>
      <span>{text}</span>
      <span aria-hidden>·</span>
      <button type="button" className="underline" style={{ color: "var(--ligus-ink)" }} onClick={onKeep}>
        Keep it
      </button>
    </span>
  );
}

/// The GitHub connection to publish through, and its login (the default owner).
function useGithub() {
  const q = useQuery({ queryKey: ["forges"], queryFn: () => forgesList(), staleTime: 60_000 });
  const gh = (q.data ?? []).find((f) => f.kind === "github" && f.hasWrite && f.available !== false) ?? null;
  return { forgeId: gh?.id ?? null, login: gh?.user ?? "" };
}

/// "Keep it as a repo": a folder (that doesn't exist yet) and an optional
/// name; then, only when asked, "Publish to GitHub (private)" behind a
/// confirmation that names the account and the repository. With a kept
/// entry, only the publish part.
export function KeepScratchDialog({ entry, onClose }: { entry: ScratchRepoDto; onClose: () => void }) {
  const qc = useQueryClient();
  const gh = useGithub();
  const kept = entry.state === "kept";
  const [path, setPath] = useState(() => suggestKeepPath(entry));
  const [name, setName] = useState("");
  const [publish, setPublish] = useState(kept);
  const [owner, setOwner] = useState<string | null>(null);
  const [repoName, setRepoName] = useState(() => scratchBaseName(entry.folder));
  const [step, setStep] = useState<"form" | "confirm">("form");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const ownerShown = (owner ?? gh.login).trim();
  const target = `${ownerShown}/${repoName.trim()}`;
  const canPublish = !publish || (ownerShown.length > 0 && repoName.trim().length > 0);

  const submit = async () => {
    setErr(null);
    if (!kept) {
      setBusy(true);
      try {
        const k = await keepScratchRepo(entry.id, path.trim(), name.trim() || null);
        toast.success(`Kept ${k.name} at ${k.path}`);
        refreshScratch(qc);
      } catch (e) {
        setErr(errMsg(e));
        return;
      } finally {
        setBusy(false);
      }
    }
    if (publish) setStep("confirm");
    else onClose();
  };

  const confirmPublish = async () => {
    setErr(null);
    setBusy(true);
    try {
      const r = await publishScratchRepo(entry.id, ownerShown, repoName.trim(), target, gh.forgeId);
      toast.success(`Published privately: ${r.url}`);
      refreshScratch(qc);
      onClose();
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const error = err && (
    <div role="alert" className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
      {err}
    </div>
  );

  if (step === "confirm") {
    return (
      <Sheet title="Publish to GitHub" side="center" width={460} onClose={onClose}>
        <div className="flex flex-col gap-3 text-sm">
          <p>
            Create the private repository <strong>{target}</strong> on GitHub and push its branches?
          </p>
          {error}
          <div className="flex justify-end gap-2">
            <Button onClick={onClose}>Not now</Button>
            <Button variant="primary" disabled={busy} onClick={() => void confirmPublish()}>
              {busy ? "Publishing…" : "Create and push"}
            </Button>
          </div>
        </div>
      </Sheet>
    );
  }

  return (
    <Sheet
      title={kept ? "Publish to GitHub" : "Keep it as a repo"}
      description={kept ? undefined : "Moves it out of the scratch folder; the cleanup no longer touches it."}
      side="center"
      width={480}
      onClose={onClose}
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        {!kept && (
          <>
            <Field label="Folder" hint="A folder that doesn't exist yet.">
              <div className="flex gap-2">
                <input
                  aria-label="Folder"
                  className="flex-1 rounded-xl border px-3 py-2 text-sm"
                  style={fieldStyle}
                  value={path}
                  onChange={(e) => setPath(e.target.value)}
                />
                <Button
                  type="button"
                  onClick={async () => {
                    const parent = await pickWorkspaceFolder();
                    if (parent) setPath(`${parent.replace(/[/\\]+$/, "")}/${scratchBaseName(entry.folder)}`);
                  }}
                >
                  Choose…
                </Button>
              </div>
            </Field>
            <Field label="Name (optional)">
              <input
                aria-label="Name"
                className="rounded-xl border px-3 py-2 text-sm"
                style={fieldStyle}
                placeholder={entry.name}
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </Field>
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={publish} onChange={(e) => setPublish(e.target.checked)} />
              Also publish to GitHub (private)
            </label>
          </>
        )}
        {publish && (
          <div className="flex gap-2">
            <Field label="Account or organization">
              <input
                aria-label="GitHub owner"
                className="rounded-xl border px-3 py-2 text-sm"
                style={fieldStyle}
                value={owner ?? gh.login}
                onChange={(e) => setOwner(e.target.value)}
              />
            </Field>
            <Field label="Repository">
              <input
                aria-label="GitHub repository name"
                className="rounded-xl border px-3 py-2 text-sm"
                style={fieldStyle}
                value={repoName}
                onChange={(e) => setRepoName(e.target.value)}
              />
            </Field>
          </div>
        )}
        {error}
        <div className="flex justify-end gap-2">
          <Button type="button" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" variant="primary" disabled={busy || (!kept && !path.trim()) || !canPublish}>
            {kept ? "Continue" : busy ? "Keeping…" : "Keep it"}
          </Button>
        </div>
      </form>
    </Sheet>
  );
}

const CLEANUP_CHOICES = [7, 14, 30, 90];

/// Settings > Repositories > Scratch repositories: this device's scratch
/// repos, Keep / Remove now, and the cleanup.
export function ScratchReposPart({ canAdmin }: { canAdmin: boolean }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: SCRATCH_KEY, queryFn: listScratchRepos });
  const [keeping, setKeeping] = useState<ScratchRepoDto | null>(null);
  const [creating, setCreating] = useState(false);
  const data: ScratchReposDto | undefined = q.data;
  if (!data) return null;
  const st = data.settings;
  const shown = data.scratch.filter((e) => e.state !== "removed");
  const value = st.cleanupDays === null || st.cleanupDays === 14 ? "default" : String(st.cleanupDays);
  const choices = st.cleanupDays && !CLEANUP_CHOICES.includes(st.cleanupDays) ? [...CLEANUP_CHOICES, st.cleanupDays].sort((a, b) => a - b) : CLEANUP_CHOICES;
  const setCleanup = async (v: string) => {
    try {
      await setScratchSettings(v === "default" ? { resetDays: true } : { cleanupDays: Number(v) });
      refreshScratch(qc);
    } catch (e) {
      toast.error(`Couldn't change the cleanup: ${errMsg(e)}`);
    }
  };
  const remove = async (e: ScratchRepoDto) => {
    const ok = await confirmDialog("Its folder is deleted from this device. Chats that used it keep their proposals and diffs.", {
      title: `Remove ${e.name} now?`,
      confirmLabel: "Remove",
      danger: true,
    });
    if (!ok) return;
    try {
      await removeScratchRepo(e.id);
      refreshScratch(qc);
    } catch (err) {
      toast.error(`Couldn't remove it: ${errMsg(err)}`);
    }
  };
  return (
    <Section
      title="Scratch repositories"
      info={`Throwaway repositories a chat works in, made on this device. Unkept ones are removed after the cleanup days without activity, with a notice ${st.noticeDays} days before.`}
      action={
        canAdmin ? (
          <Button
            size="sm"
            disabled={creating}
            onClick={async () => {
              setCreating(true);
              try {
                const e = await createScratchRepo(null);
                toast.success(`Made ${e.name}`);
                refreshScratch(qc);
              } catch (err) {
                toast.error(`Couldn't make a scratch repo: ${errMsg(err)}`);
              } finally {
                setCreating(false);
              }
            }}
          >
            New scratch repo
          </Button>
        ) : undefined
      }
    >
      {shown.length === 0 ? (
        <EmptyHint text="No scratch repositories" />
      ) : (
        <RowList label="Scratch repositories">
          {shown.map((e) => (
            <ListRow
              key={e.id}
              title={e.name}
              meta={
                e.state === "kept"
                  ? e.published
                    ? "kept · published"
                    : "kept"
                  : e.removeAt
                    ? `removed on ${formatScratchDate(e.removeAt)}`
                    : "no cleanup"
              }
              actionsLabel={`Actions for ${e.name}`}
              trailing={
                <>
                  {e.state === "live" && <ScratchTag />}
                  {e.state === "live" && (
                    <Button size="sm" variant="quiet" onClick={() => setKeeping(e)}>
                      Keep…
                    </Button>
                  )}
                </>
              }
              actions={[
                ...(e.state === "kept" && !e.published ? [{ label: "Publish to GitHub…", onSelect: () => setKeeping(e) }] : []),
                ...(e.state === "live" ? [{ label: "Remove now", danger: true, onSelect: () => void remove(e) }] : []),
              ]}
            />
          ))}
        </RowList>
      )}
      <label className="flex items-center gap-2 px-1 text-sm">
        <span className="shrink-0">Remove unkept ones after</span>
        <span className="w-44">
          <SelectField ariaLabel="Scratch cleanup" value={value} onChange={(v) => void setCleanup(v)}>
            <option value="default">14 days (default)</option>
            {choices
              .filter((d) => d !== 14)
              .map((d) => (
                <option key={d} value={String(d)}>
                  {d} days
                </option>
              ))}
            <option value="0">Never (off)</option>
          </SelectField>
        </span>
      </label>
      {keeping && <KeepScratchDialog entry={keeping} onClose={() => setKeeping(null)} />}
    </Section>
  );
}
