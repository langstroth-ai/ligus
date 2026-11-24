import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  forgesAdd,
  forgesClone,
  forgesDeviceStart,
  forgesDeviceStatus,
  forgesList,
  forgesRemove,
  forgesRepos,
  forgesSetToken,
  forgesTest,
  openExternal,
  pickWorkspaceFolder,
  setWorkspaceRoot,
  type ForgeConnection,
  type ForgeDeviceStart,
} from "@/lib/ipc";
import { confirmDialog } from "@/components/Dialog";
import { Button, EmptyHint, FormDisclosure, ListRow, RowList, Section, fieldStyle } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { useRowMenu } from "@/components/RowMenu";
import { toast, errMsg } from "@/components/Toast";

/// The kinds a person picks from, with where each one lives by default.
export const FORGE_KINDS: { kind: string; label: string; base: string }[] = [
  { kind: "github", label: "GitHub", base: "https://github.com" },
  { kind: "gitlab", label: "GitLab", base: "https://gitlab.com" },
  { kind: "codeberg", label: "Codeberg", base: "https://codeberg.org" },
  { kind: "forgejo", label: "Forgejo (your server)", base: "https://" },
  { kind: "gitea", label: "Gitea (your server)", base: "https://" },
];

/// What a connection's credential is, in words.
export function authLabel(c: ForgeConnection): string {
  switch (c.authSource) {
    case "gh":
      return "GitHub CLI login (gh) on this device";
    case "oauth":
      return "device sign-in";
    case "token":
      return c.hasWrite ? "token (read and write)" : c.hasRead ? "token (read only)" : "no token yet";
    default:
      return "anonymous (public repositories only)";
  }
}

/// Settings → Code hosts: where this device does repo work. Not identity:
/// Ligus doesn't use any of this to sign you in.
export function CodeHostsSection() {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["forges"], queryFn: () => forgesList(false) });
  const refresh = () => qc.invalidateQueries({ queryKey: ["forges"] });
  const [adding, setAdding] = useState(false);

  return (
    <div>
      <Section
        title="Connections"
        action={
          <Button size="sm" onClick={() => setAdding(true)}>
            Connect a code host
          </Button>
        }
        info="Code hosts are for repository work on this device: listing, cloning and opening pull or merge requests. Ligus doesn't use them to sign you in. Tokens stay on this device, in its secret store. They are never synced and never sent to your relay."
      >
        <p className="text-xs text-ink-soft">Tokens stay on this device and are never synced.</p>
        {list.isLoading && <p className="text-sm text-ink-soft">Loading…</p>}
        <RowList label="Code hosts">
          {(list.data ?? []).map((c) => (
            <ConnectionRow key={c.id} c={c} onChange={refresh} />
          ))}
        </RowList>
        {list.data && list.data.length === 0 && <EmptyHint text="No code hosts yet" />}
      </Section>
      {adding && (
        <AddConnection
          onClose={() => setAdding(false)}
          onAdded={() => {
            setAdding(false);
            refresh();
          }}
        />
      )}
      {(list.data ?? []).length > 0 && <RepoPicker connections={list.data ?? []} />}
    </div>
  );
}

function ConnectionRow({ c, onChange }: { c: ForgeConnection; onChange: () => void }) {
  const [status, setStatus] = useState<ForgeConnection | null>(null);
  const [token, setToken] = useState("");
  const [purpose, setPurpose] = useState<"read" | "write">("write");
  const [device, setDevice] = useState<ForgeDeviceStart | null>(null);
  // A connection without a credential opens with its token field showing.
  const [editing, setEditing] = useState(!c.hasRead && !c.hasWrite && c.authSource !== "gh");
  const shown = status ?? c;

  async function test() {
    try {
      setStatus(await forgesTest(c.id));
      onChange();
    } catch (e) {
      toast.error(`Couldn't reach ${c.displayName}: ${errMsg(e)}`);
    }
  }

  async function saveToken() {
    try {
      await forgesSetToken(c.id, purpose, token);
      setToken("");
      toast.success(`Saved the ${purpose} token for ${c.displayName} on this device.`);
      onChange();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function signIn() {
    try {
      const d = await forgesDeviceStart(c.id);
      setDevice(d);
      void openExternal(d.verificationUri);
      const poll = async () => {
        const s = await forgesDeviceStatus().catch(() => null);
        if (!s || s.state === "pending") {
          window.setTimeout(poll, 3000);
          return;
        }
        setDevice(null);
        if (s.state === "done") {
          toast.success(`Signed in to ${c.displayName}.`);
          onChange();
        } else {
          toast.error(`Sign-in ${s.state}${s.error ? `: ${s.error}` : ""}`);
        }
      };
      window.setTimeout(poll, 3000);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function remove() {
    const ok = await confirmDialog("This removes the connection and deletes its tokens from this device's secret store.", {
      title: `Remove ${c.displayName}?`,
      confirmLabel: "Remove",
      danger: true,
    });
    if (!ok) return;
    try {
      await forgesRemove(c.id);
      onChange();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  const rm = useRowMenu(`Actions for ${c.displayName}`, [
    { label: "Test", onSelect: () => void test() },
    ...(c.deviceFlow && c.authSource !== "gh" ? [{ label: "Sign in", onSelect: () => void signIn() }] : []),
    ...(c.authSource !== "gh" ? [{ label: editing ? "Hide token field" : "Set a token…", onSelect: () => setEditing((v) => !v) }] : []),
    { label: "Remove", danger: true, onSelect: () => void remove() },
  ]);
  return (
    <li className="px-1" data-testid={`forge-${c.id}`} {...rm.rowProps}>
      <div className="ligus-listrow flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-baseline gap-1.5">
          <span className="shrink-0 text-[13px] font-medium">{c.displayName}</span>
          <span className="truncate text-[11.5px] text-ink-soft">
            {c.baseUrl} · {authLabel(c)} · held by {c.holder}
            {shown.user ? ` · as ${shown.user}` : ""}
          </span>
        </div>
        <span className="flex shrink-0 items-center gap-2">
          {/* Not signed in yet: the one action it needs stays visible. */}
          {c.deviceFlow && c.authSource !== "gh" && !c.hasRead && !c.hasWrite && (
            <Button size="sm" onClick={() => void signIn()}>
              Sign in
            </Button>
          )}
          {rm.button}
        </span>
      </div>
      {shown.available === false && shown.reason && <div className="pb-1 text-xs text-ink-soft">{shown.reason}</div>}
      {device && (
        <p className="pb-2 text-sm">
          Enter <span className="font-mono font-semibold">{device.userCode}</span> at{" "}
          <button className="underline" onClick={() => void openExternal(device.verificationUri)}>
            {device.verificationUri}
          </button>
          . Check that the page names the Ligus app before you approve.
        </p>
      )}
      {editing && c.authSource !== "gh" && (
        <div className="flex gap-2 pb-2">
          <select
            aria-label="Token purpose"
            value={purpose}
            onChange={(e) => setPurpose(e.target.value as "read" | "write")}
            className="rounded-xl border px-2 py-1 text-sm"
            style={fieldStyle}
          >
            <option value="write">Write token</option>
            <option value="read">Read token</option>
          </select>
          <input
            type="password"
            autoComplete="off"
            aria-label={`${c.displayName} token`}
            placeholder="Paste a token"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            className="w-full rounded-xl border px-3 py-1 font-mono text-sm"
            style={fieldStyle}
          />
          <Button size="sm" variant="primary" disabled={!token.trim()} onClick={() => void saveToken()}>
            Save
          </Button>
        </div>
      )}
      {rm.menu}
    </li>
  );
}

function AddConnection({ onAdded, onClose }: { onAdded: () => void; onClose: () => void }) {
  const [more, setMore] = useState(false);
  const [kind, setKind] = useState("gitlab");
  const [base, setBase] = useState("https://gitlab.com");
  const [clientId, setClientId] = useState("");
  const [internal, setInternal] = useState(false);

  async function add() {
    try {
      await forgesAdd(kind, base, kind === "github" && base.trim() === "https://github.com" ? "gh" : "token", clientId || undefined, internal || undefined);
      onAdded();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  return (
    <Sheet
      title="Connect a code host"
      side="center"
      width={480}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={() => void add()}>
            Add
          </Button>
        </>
      }
    >
      <div className="space-y-2">
        <div className="flex gap-2">
          <select
            aria-label="Code host kind"
            value={kind}
            onChange={(e) => {
              setKind(e.target.value);
              setBase(FORGE_KINDS.find((k) => k.kind === e.target.value)?.base ?? "https://");
            }}
            className="rounded-xl border px-2 py-1 text-sm"
            style={fieldStyle}
          >
            {FORGE_KINDS.map((k) => (
              <option key={k.kind} value={k.kind}>
                {k.label}
              </option>
            ))}
          </select>
          <input
            aria-label="Code host address"
            value={base}
            onChange={(e) => setBase(e.target.value)}
            className="w-full rounded-xl border px-3 py-1 font-mono text-sm"
            style={fieldStyle}
          />
        </div>
        <FormDisclosure label="More options" open={more} onToggle={() => setMore((v) => !v)}>
          <div className="space-y-2">
            {(kind === "gitlab" || kind === "github") && (
              <input
                aria-label="OAuth application id (self-managed only)"
                placeholder="Application id for device sign-in (self-managed servers only; optional)"
                value={clientId}
                onChange={(e) => setClientId(e.target.value)}
                className="w-full rounded-xl border px-3 py-1 font-mono text-sm"
                style={fieldStyle}
              />
            )}
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={internal} onChange={(e) => setInternal(e.target.checked)} />
              Internal server on this device's network (allows a local or private address; this device only)
            </label>
          </div>
        </FormDisclosure>
      </div>
    </Sheet>
  );
}

/// Repo picker across code hosts: pick a connection, search, clone into a
/// folder, and use it as this workspace's folder.
function RepoPicker({ connections }: { connections: ForgeConnection[] }) {
  const [id, setId] = useState(connections[0]?.id ?? "");
  const [q, setQ] = useState("");
  const [search, setSearch] = useState("");
  const qc = useQueryClient();
  const repos = useQuery({
    queryKey: ["forge-repos", id, search],
    queryFn: () => forgesRepos(id, search || undefined),
    enabled: !!id,
    retry: false,
  });

  async function clone(full: string) {
    try {
      const parent = await pickWorkspaceFolder();
      if (!parent) return;
      const path = await forgesClone(id, full, parent);
      const use = await confirmDialog(`${full} is at ${path}. Use it as this workspace's folder?`, {
        title: "Cloned",
        confirmLabel: "Use it",
      });
      if (use) {
        await setWorkspaceRoot(path);
        qc.invalidateQueries({ queryKey: ["settings"] });
        qc.invalidateQueries({ queryKey: ["git-status"] });
      }
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  return (
    <Section title="Repositories">
      <div className="flex gap-2">
        <select aria-label="Code host" value={id} onChange={(e) => setId(e.target.value)} className="rounded-xl border px-2 py-1 text-sm" style={fieldStyle}>
          {connections.map((c) => (
            <option key={c.id} value={c.id}>
              {c.displayName}
            </option>
          ))}
        </select>
        <input
          aria-label="Search repositories"
          placeholder="Search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && setSearch(q.trim())}
          className="w-full rounded-xl border px-3 py-1 text-sm"
          style={fieldStyle}
        />
      </div>
      {repos.isError && <p className="text-sm text-ink-soft">{errMsg(repos.error)}</p>}
      <RowList label="Repositories">
        {(repos.data?.repos ?? []).map((r) => {
          const full = `${r.repo.owner}/${r.repo.name}`;
          return (
            <ListRow
              key={full}
              title={full}
              meta={[r.private ? "private" : "", r.fork ? "fork" : "", r.description].filter(Boolean).join(" · ")}
              tip={r.description || undefined}
              trailing={
                <Button size="sm" onClick={() => void clone(full)}>
                  Clone…
                </Button>
              }
            />
          );
        })}
      </RowList>
    </Section>
  );
}
