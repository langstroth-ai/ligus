import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  acceptRepoAlias,
  addWorkspaceRepo,
  bindRepoCheckout,
  cloneWorkspaceRepo,
  listWorkspaceRepos,
  pickWorkspaceFolder,
  removeWorkspaceRepo,
  renameWorkspaceRepo,
  rescanRepos,
  setHostAlias,
  setRepoAliases,
  setRepoCoverageReporting,
  unbindRepoCheckout,
  type HostAliasDto,
  type RepoHostCoverage,
  type WorkspaceRepoDto,
  type WorkspaceReposDto,
} from "@/lib/ipc";
import { confirmDialog, promptDialog } from "@/components/Dialog";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

export const REPOS_KEY = ["workspace-repos"];

/// A coverage state in words.
export function coverageLabel(state: string, viaFork = false): string {
  const fork = viaFork ? " (via a fork)" : "";
  switch (state) {
    case "present":
      return `checked out${fork}`;
    case "cloning":
      return "cloning…";
    case "missing":
      return "not checked out";
    case "error":
      return "checkout no longer matches";
    case "removed":
      return "removed";
    default:
      return "unknown (doesn't report)";
  }
}

function HostChip({ h }: { h: RepoHostCoverage }) {
  const ok = h.state === "present";
  return (
    <span
      className="rounded-full border px-2 py-0.5 text-xs"
      style={{ borderColor: "var(--ligus-line)", opacity: h.online ? 1 : 0.6 }}
      title={h.online ? "online" : "offline"}
    >
      {ok ? "✓ " : ""}
      {h.label}: {coverageLabel(h.state, h.viaFork)}
    </span>
  );
}

/// "This folder is github.com/zazulam/wefit. Add WeFit to the workspace?"
export function RepoSuggestion({ data, onDone }: { data: WorkspaceReposDto; onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const s = data.suggestion;
  if (!s || dismissed) return null;
  return (
    <div className="mb-4 rounded-2xl border p-4 text-sm" style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}>
      <div>
        This folder is <code>{s.canonical}</code>
        {s.viaFork ? " (through your fork)" : ""}.{" "}
        {data.canAdmin
          ? `Add ${s.name} to the workspace so your other devices and your phone can find it?`
          : `Ask an admin to add ${s.name} to the workspace so your other devices and your phone can find it.`}
      </div>
      {data.canAdmin && (
        <div className="mt-3 flex gap-2">
          <Button
            variant="primary"
            size="sm"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                await addWorkspaceRepo(s.canonical, { name: s.name, path: s.path });
                toast.success(`Added ${s.name} to the workspace`);
                onDone();
              } catch (e) {
                toast.error(`Couldn't add it: ${errMsg(e)}`);
              } finally {
                setBusy(false);
              }
            }}
          >
            Add
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setDismissed(true)}>
            Not now
          </Button>
        </div>
      )}
    </div>
  );
}

/// A synced alias: "used here" once a person on this device accepted it,
/// else an Accept button. Accepting changes only identity matching here.
function AliasChip({ alias, accepted, onToggle }: { alias: string; accepted: boolean; onToggle: (accept: boolean) => void }) {
  return (
    <span className="flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs" style={{ borderColor: "var(--ligus-line)" }}>
      <code>{alias}</code>
      {accepted ? (
        <>
          <span className="text-ink-soft">used here</span>
          <button type="button" className="underline" onClick={() => onToggle(false)} aria-label={`Stop using ${alias} here`}>
            stop
          </button>
        </>
      ) : (
        <button type="button" className="underline" onClick={() => onToggle(true)} aria-label={`Accept ${alias} here`}>
          accept here
        </button>
      )}
    </span>
  );
}

function RepoRow({ r, data, refresh }: { r: WorkspaceRepoDto; data: WorkspaceReposDto; refresh: () => void }) {
  const [cloning, setCloning] = useState(false);
  const run = async (what: string, f: () => Promise<unknown>) => {
    try {
      await f();
      refresh();
    } catch (e) {
      toast.error(`${what}: ${errMsg(e)}`);
    }
  };
  return (
    <div className="border-b py-3 last:border-b-0" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="flex items-baseline justify-between gap-3">
        <div className="min-w-0">
          <div className="font-medium">
            {r.name} {r.removed && <span className="text-xs text-ink-soft">(removed from this workspace)</span>}
          </div>
          <div className="truncate text-xs text-ink-soft">
            {r.canonical}
            {r.defaultBranch ? ` · ${r.defaultBranch}` : ""}
          </div>
          {r.lookalikeOf && (
            <div className="text-xs" style={{ color: "var(--ligus-accent-warm, orange)" }}>
              Same history as {r.lookalikeOf}, different owner. Check the full name before cloning it.
            </div>
          )}
        </div>
        {data.canAdmin && !r.removed && (
          <div className="flex shrink-0 gap-1">
            <Button
              size="sm"
              variant="ghost"
              onClick={async () => {
                const list = await promptDialog(
                  "Other names of this repository (comma separated): an old owner, or a different case on a case-sensitive server. Names only; each device uses one after someone there accepts it.",
                  { defaultValue: (r.aliases ?? []).map((a) => a.alias).join(", ") },
                );
                if (list === null) return;
                const aliases = list.split(",").map((a) => a.trim()).filter(Boolean);
                await run("Couldn't set the aliases", () => setRepoAliases(r.id, aliases));
              }}
            >
              Aliases…
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={async () => {
                const name = await promptDialog("Repository name", { defaultValue: r.name });
                if (name && name.trim() && name !== r.name) await run("Couldn't rename it", () => renameWorkspaceRepo(r.id, name.trim()));
              }}
            >
              Rename
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={async () => {
                const ok = await confirmDialog("Chats that use it keep naming it. Checkouts on devices stay where they are.", {
                  title: `Remove ${r.name}?`,
                  confirmLabel: "Remove",
                  danger: true,
                });
                if (ok) await run("Couldn't remove it", () => removeWorkspaceRepo(r.id));
              }}
            >
              Remove
            </Button>
          </div>
        )}
      </div>
      {!r.removed && (
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="text-ink-soft">{data.hostLabel}:</span>
          {r.here.path ? (
            <>
              <span>
                ✓ <code className="text-xs">{r.here.path}</code>
                {r.here.viaFork ? " (via a fork)" : ""}
                {r.here.viaAlias ? " (via alias)" : ""}
              </span>
              {r.here.state === "error" && <span className="text-xs">now points elsewhere; it no longer counts as {r.name}</span>}
              <Button size="sm" variant="ghost" onClick={() => run("Couldn't unbind it", () => unbindRepoCheckout(r.id, r.here.path!))}>
                Unbind
              </Button>
            </>
          ) : (
            <>
              <span>{coverageLabel(r.here.state)}</span>
              <Button
                size="sm"
                onClick={async () => {
                  const p = await pickWorkspaceFolder();
                  if (p) await run("Couldn't bind that folder", () => bindRepoCheckout(r.id, p));
                }}
              >
                Bind folder…
              </Button>
              {!r.canonical.startsWith("local:") && r.here.state !== "cloning" && (
                <Button
                  size="sm"
                  disabled={cloning}
                  title={`Clone ${r.canonical} into a folder you pick, through this device's connection to its host`}
                  onClick={async () => {
                    const parent = await pickWorkspaceFolder();
                    if (!parent) return;
                    setCloning(true);
                    try {
                      const c = await cloneWorkspaceRepo(r.id, parent);
                      toast.success(`Cloned ${r.name} into ${c.path}.`);
                      if (c.rootMatches === false) {
                        toast.error(`${r.name}'s clone doesn't contain the first commit the workspace entry names. Check it's the right repository.`);
                      }
                      refresh();
                    } catch (e) {
                      toast.error(`Couldn't clone ${r.name}: ${errMsg(e)}`);
                    } finally {
                      setCloning(false);
                    }
                  }}
                >
                  {cloning ? "Cloning…" : "Clone into…"}
                </Button>
              )}
            </>
          )}
          {r.here.found.map((f) => (
            <span key={f} className="flex items-center gap-1 text-xs">
              Found at <code>{f}</code>.
              <Button size="sm" variant="primary" onClick={() => run("Couldn't use it", () => bindRepoCheckout(r.id, f))}>
                Use it
              </Button>
            </span>
          ))}
        </div>
      )}
      {!r.removed && (r.aliases ?? []).length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5 text-xs">
          <span className="text-ink-soft">Also known as:</span>
          {(r.aliases ?? []).map((a) => (
            <AliasChip
              key={a.alias}
              alias={a.alias}
              accepted={a.accepted}
              onToggle={(accept) => void run("Couldn't change it", () => acceptRepoAlias(a.alias, accept, r.id))}
            />
          ))}
        </div>
      )}
      {!r.removed && r.hosts.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1.5">
          {r.hosts.map((h) => (
            <HostChip key={h.hostId} h={h} />
          ))}
        </div>
      )}
    </div>
  );
}

/// Settings → Repositories: what code this workspace works on, where it is
/// on this device, and which other hosts have it. Paths shown are this
/// device's own; nothing here leaves the device except the repo list
/// itself (visible to every member) and, when on, which repos this device
/// has checked out.
export function RepositoriesSection() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: REPOS_KEY, queryFn: listWorkspaceRepos });
  const refresh = () => void qc.invalidateQueries({ queryKey: REPOS_KEY });
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const data = q.data;

  return (
    <div>
      <p className="mb-4 text-sm text-ink-soft">
        The workspace&rsquo;s repositories are shared with every member. Where each one lives on a device stays on
        that device; other hosts show only whether they have it checked out.
      </p>
      {q.isError && <div className="text-sm">Couldn&rsquo;t load repositories: {errMsg(q.error)}</div>}
      {data && <RepoSuggestion data={data} onDone={refresh} />}
      <Section title="Repositories">
        {data && data.repos.length === 0 && (
          <div className="text-sm text-ink-soft">This workspace has no repositories yet.</div>
        )}
        {data?.repos.map((r) => <RepoRow key={r.id} r={r} data={data} refresh={refresh} />)}
        {data && (data.canAdmin ? (
          <form
            className="mt-3 flex gap-2"
            onSubmit={async (e) => {
              e.preventDefault();
              if (!input.trim()) return;
              setBusy(true);
              try {
                const r = await addWorkspaceRepo(input.trim());
                toast.success(`Added ${r.name} (${r.canonical})`);
                setInput("");
                refresh();
              } catch (err) {
                toast.error(`Couldn't add it: ${errMsg(err)}`);
              } finally {
                setBusy(false);
              }
            }}
          >
            <input
              aria-label="Repository URL"
              className="flex-1 rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
              placeholder="github.com/owner/name or a git remote URL"
              value={input}
              onChange={(e) => setInput(e.target.value)}
            />
            <Button type="submit" variant="primary" disabled={busy || !input.trim()}>
              Add repository
            </Button>
          </form>
        ) : (
          <div className="mt-3 text-sm text-ink-soft">Ask an admin to add a repository.</div>
        ))}
      </Section>
      {data && <HostAliasesSection data={data} refresh={refresh} />}
      {data && (
        <Section title="On this device">
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <Button
              size="sm"
              onClick={async () => {
                const p = await pickWorkspaceFolder();
                if (!p) return;
                try {
                  const n = await rescanRepos(p);
                  toast.success(n ? `Found ${n} checkout${n === 1 ? "" : "s"}` : "No checkouts of this workspace's repositories there");
                  refresh();
                } catch (e) {
                  toast.error(errMsg(e));
                }
              }}
            >
              Scan a folder…
            </Button>
            <Button size="sm" variant="ghost" onClick={async () => { await rescanRepos(); refresh(); }}>
              Rescan
            </Button>
            {data.scanRoots.length > 0 && <span className="text-xs text-ink-soft">Scanning: {data.scanRoots.join(", ")}</span>}
          </div>
          <label className="mt-3 flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={data.coverageReporting}
              onChange={async (e) => {
                await setRepoCoverageReporting(e.target.checked);
                refresh();
              }}
            />
            Tell other members which of these repositories this device has checked out (never where)
          </label>
        </Section>
      )}
    </div>
  );
}

/// Settings → Repositories → Host aliases: SSH hosts a self-hosted forge
/// serves from, mapped to its host for identity matching. Synced (Admin,
/// with a passkey); each device uses one only after someone there accepts
/// it. Never where git connects.
export function HostAliasesSection({ data, refresh }: { data: WorkspaceReposDto; refresh: () => void }) {
  const [alias, setAlias] = useState("");
  const [host, setHost] = useState("");
  const list: HostAliasDto[] = data.hostAliases ?? [];
  if (!data.canAdmin && list.length === 0) return null;
  const run = async (what: string, f: () => Promise<unknown>) => {
    try {
      await f();
      refresh();
    } catch (e) {
      toast.error(`${what}: ${errMsg(e)}`);
    }
  };
  return (
    <Section title="Host aliases">
      <p className="mb-2 text-xs text-ink-soft">
        Another name for a repository host, such as the SSH host a self-hosted forge uses. It changes only how this
        workspace recognizes checkouts, never where git connects, and each device uses it only after someone there
        accepts it.
      </p>
      {list.map((h) => (
        <div key={h.alias} className="flex flex-wrap items-center gap-2 py-1 text-sm">
          <code>{h.alias}</code> → <code>{h.host}</code>
          <AliasChip alias={h.alias} accepted={h.accepted} onToggle={(accept) => void run("Couldn't change it", () => acceptRepoAlias(h.alias, accept, h.host))} />
          {data.canAdmin && (
            <Button size="sm" variant="ghost" onClick={() => void run("Couldn't remove it", () => setHostAlias(h.alias, ""))}>
              Remove
            </Button>
          )}
        </div>
      ))}
      {data.canAdmin && (
        <form
          className="mt-2 flex gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            if (!alias.trim() || !host.trim()) return;
            await run("Couldn't add it", () => setHostAlias(alias.trim(), host.trim()));
            setAlias("");
            setHost("");
          }}
        >
          <input
            aria-label="Alias host"
            className="flex-1 rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
            placeholder="ssh.git.example.com"
            value={alias}
            onChange={(e) => setAlias(e.target.value)}
          />
          <input
            aria-label="Repository host"
            className="flex-1 rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
            placeholder="git.example.com"
            value={host}
            onChange={(e) => setHost(e.target.value)}
          />
          <Button type="submit" disabled={!alias.trim() || !host.trim()}>
            Add alias
          </Button>
        </form>
      )}
    </Section>
  );
}

/// The "Add WeFit to the workspace?" prompt after a folder is bound, shown
/// anywhere in the app (bottom left) until added or dismissed.
export function RepoSuggestionBanner() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: REPOS_KEY, queryFn: listWorkspaceRepos, retry: false });
  if (!q.data?.suggestion) return null;
  return (
    <div className="fixed bottom-4 left-4 z-[60] w-[360px] shadow-xl" role="status" data-calm="banner">
      <RepoSuggestion data={q.data} onDone={() => void qc.invalidateQueries({ queryKey: REPOS_KEY })} />
    </div>
  );
}
