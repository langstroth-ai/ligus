import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  identityLinkPrepare,
  identityLinkPublish,
  identityLinkRemove,
  identityLinkSettings,
  identityLinkSignWithAgent,
  identityLinkStatuses,
  listWorkspaces,
  myIdentityLinks,
  myProfile,
  setIdentityLinkSettings,
  type LinkDraftDto,
  type LinkMethod,
  type SelfHostedForge,
} from "@/lib/ipc";
import { confirmDialog } from "@/components/Dialog";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { LinkChips } from "@/components/LinkChips";
import { FORGES, forgeLabel, normalizeHandle } from "@/lib/links";

/// The disclosure the published-key fallback carries (owner decision D15).
export const KEY_PUBLISHED_DISCLOSURE =
  "This puts your Ligus account key on your forge profile, where anyone can see it. It links your Ligus account to that forge account publicly, for as long as the key stays there. It grants no access: it goes on the signing keys list, not the SSH (push) keys.";

/// Settings → Profile → Linked accounts: show that you are also someone on
/// a code forge, per workspace, proven with an SSH key the forge already
/// lists for you. Off until you add one.
export function LinkedAccountsSection() {
  const qc = useQueryClient();
  const profile = useQuery({ queryKey: ["my-profile"], queryFn: myProfile });
  const links = useQuery({ queryKey: ["my-links"], queryFn: myIdentityLinks });
  const [adding, setAdding] = useState(false);
  const [checking, setChecking] = useState(false);

  async function checkNow() {
    if (!profile.data?.accountId) return;
    setChecking(true);
    try {
      const fresh = await identityLinkStatuses(profile.data.accountId, true);
      qc.setQueryData(["my-links"], fresh);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setChecking(false);
    }
  }

  async function unlink(linkId: string, label: string) {
    const ok = await confirmDialog(`Unlink ${label}? It is removed from every workspace it is shown in.`, { danger: true });
    if (!ok) return;
    try {
      const n = await identityLinkRemove(linkId);
      toast.success(n > 0 ? `Unlinked from ${n} workspace${n === 1 ? "" : "s"}.` : "Unlinked.");
      qc.invalidateQueries({ queryKey: ["my-links"] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  const list = links.data ?? [];
  return (
    <Section
      title="Linked accounts"
      action={
        !adding ? (
          <div className="flex gap-2">
            {list.length > 0 && (
              <Button size="sm" disabled={checking} onClick={() => void checkNow()}>
                {checking ? "Checking…" : "Check now"}
              </Button>
            )}
            <Button size="sm" onClick={() => setAdding(true)}>
              Link an account
            </Button>
          </div>
        ) : undefined
      }
    >
      <p className="text-xs text-ink-soft">
        Show the members of a workspace that you are also someone on GitHub, GitLab, Codeberg, Forgejo, Gitea,
        SourceHut or Tangled. You choose the workspaces; their members and agents can see it. Every device checks it
        with the forge itself, never through your relay. A link helps people recognize you; it never lets anyone in
        or vouches for their keys.
      </p>
      {list.map((l) => (
        <div key={l.linkId} className="flex items-start justify-between gap-3 rounded-xl border px-3 py-2" style={fieldStyle}>
          <div className="min-w-0 space-y-1">
            <LinkChips links={[l]} />
            <div className="text-[11px] text-ink-soft">
              In {l.workspaces.map((w) => w.name).join(", ") || "no workspace"}
              {l.discoverable && " · on your contact card"} · until {l.expiresAt.slice(0, 10)}
              {l.method === "keyPublished" && " · Ligus key on the forge"}
            </div>
          </div>
          <button
            className="shrink-0 text-xs hover:opacity-80"
            style={{ color: "var(--hive-danger-ink)" }}
            onClick={() => void unlink(l.linkId, `${forgeLabel(l.provider, l.host)} ${l.handle}`)}
          >
            Unlink
          </button>
        </div>
      ))}
      {adding && (
        <LinkFlow
          onDone={() => {
            setAdding(false);
            qc.invalidateQueries({ queryKey: ["my-links"] });
            qc.invalidateQueries({ queryKey: ["co-members"] });
          }}
        />
      )}
      <ForgeCheckSettings />
    </Section>
  );
}

function LinkFlow({ onDone }: { onDone: () => void }) {
  const workspaces = useQuery({ queryKey: ["workspaces"], queryFn: listWorkspaces });
  const settings = useQuery({ queryKey: ["link-settings"], queryFn: identityLinkSettings });
  const forges = [
    ...FORGES,
    ...(settings.data?.forges ?? []).map((f) => ({
      provider: f.provider,
      host: f.host,
      label: `${forgeLabel(f.provider, f.host)}`,
      keyPublished: f.provider === "github" || f.provider === "gitlab",
      hint: "your username",
    })),
  ];
  const [forgeIdx, setForgeIdx] = useState(0);
  const forge = forges[forgeIdx] ?? FORGES[0];
  const [handle, setHandle] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [discoverable, setDiscoverable] = useState(false);
  const [method, setMethod] = useState<LinkMethod>("sshSig");
  const [draft, setDraft] = useState<LinkDraftDto | null>(null);
  const [sigs, setSigs] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const rooms = (workspaces.data ?? []).filter((w) => w.kind === "room");

  async function prepare() {
    setBusy(true);
    try {
      const d = await identityLinkPrepare({
        provider: forge.provider,
        host: forge.host,
        handle: normalizeHandle(forge.provider, handle),
        workspaceIds: picked,
        method,
        discoverable,
      });
      setDraft(d);
      setSigs({});
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function signWithAgent(key: string) {
    if (!draft) return;
    setBusy(true);
    try {
      setSigs(await identityLinkSignWithAgent(draft.linkId, key));
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function publish() {
    if (!draft) return;
    setBusy(true);
    try {
      const r = await identityLinkPublish(draft.linkId, sigs);
      toast.success(
        r.status.state === "verified"
          ? `Linked: ${forgeLabel(draft.provider, draft.host)} lists your key.`
          : "Linked. Others will check it with the forge.",
      );
      onDone();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  if (draft) {
    const allSigned = draft.method === "keyPublished" || draft.statements.every((s) => (sigs[s.workspaceId] ?? "").trim());
    return (
      <div className="space-y-3 rounded-xl border px-3 py-3 text-xs" style={fieldStyle}>
        <div>
          <strong>{forgeLabel(draft.provider, draft.host)}</strong>{" "}
          <span className="font-mono">{draft.handle}</span> <span className="text-ink-soft">(id {draft.subjectId})</span>
        </div>
        {draft.method === "keyPublished" ? (
          <div className="space-y-2">
            <p style={{ color: "var(--hive-danger-ink)" }}>{KEY_PUBLISHED_DISCLOSURE}</p>
            <p>
              Add this as a <strong>signing key</strong> on {forgeLabel(draft.provider, draft.host)} (GitHub: Settings → SSH
              and GPG keys → New SSH key → Key type "Signing Key"; GitLab: SSH Keys → Usage type "Signing"), then publish.
            </p>
            <textarea readOnly rows={2} value={draft.accountKeyOpenssh ?? ""} className="w-full rounded-lg border px-2 py-1 font-mono text-[11px]" style={fieldStyle} />
            <Button size="sm" onClick={() => void navigator.clipboard.writeText(draft.accountKeyOpenssh ?? "")}>
              Copy key
            </Button>
          </div>
        ) : (
          <div className="space-y-2">
            <p>
              Sign each statement with an SSH key {forgeLabel(draft.provider, draft.host)} lists for you. Ligus only ever
              asks for signatures under the namespace <code>{draft.namespace}</code>, so this can't be reused as a git or
              file signature.
            </p>
            {draft.agentKeys.length > 0 && (
              <div className="flex flex-wrap gap-2">
                {draft.agentKeys.map((k) => (
                  <Button key={k} size="sm" variant="primary" disabled={busy} onClick={() => void signWithAgent(k)} title={k}>
                    Sign with {k.split(" ")[2] || k.split(" ")[0]} (ssh-agent)
                  </Button>
                ))}
              </div>
            )}
            <p className="text-ink-soft">
              Or run, for each statement (saved to a file, or pasted on stdin), and paste what it prints:
              <code className="mt-1 block whitespace-pre-wrap font-mono">{draft.signCommand} &lt; statement.txt</code>
            </p>
            {draft.statements.map((s) => (
              <div key={s.workspaceId} className="space-y-1">
                <div className="font-medium">{s.workspaceName}</div>
                <pre className="overflow-x-auto rounded-lg border p-2 font-mono text-[11px]" style={fieldStyle} aria-label={`Statement for ${s.workspaceName}`}>
                  {s.statement}
                </pre>
                <Button size="sm" onClick={() => void navigator.clipboard.writeText(s.statement)}>
                  Copy statement
                </Button>
                <textarea
                  rows={3}
                  value={sigs[s.workspaceId] ?? ""}
                  onChange={(e) => setSigs({ ...sigs, [s.workspaceId]: e.target.value })}
                  placeholder="-----BEGIN SSH SIGNATURE-----"
                  className="w-full rounded-lg border px-2 py-1 font-mono text-[11px]"
                  style={fieldStyle}
                  aria-label={`Signature for ${s.workspaceName}`}
                />
              </div>
            ))}
          </div>
        )}
        <div className="flex gap-2">
          <Button variant="primary" size="md" disabled={busy || !allSigned} onClick={() => void publish()}>
            Publish
          </Button>
          <Button size="md" onClick={onDone}>
            Cancel
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-2 rounded-xl border px-3 py-3 text-xs" style={fieldStyle}>
      <label className="block text-ink-soft">Forge</label>
      <select
        value={forgeIdx}
        onChange={(e) => {
          setForgeIdx(Number(e.target.value));
          setMethod("sshSig");
        }}
        className="w-full rounded-lg border px-2 py-1"
        style={fieldStyle}
        aria-label="Forge"
      >
        {forges.map((f, i) => (
          <option key={`${f.provider}/${f.host}`} value={i}>
            {f.label}
          </option>
        ))}
      </select>
      <label className="block text-ink-soft">Account</label>
      <input
        value={handle}
        onChange={(e) => setHandle(e.target.value)}
        placeholder={forge.hint}
        className="w-full rounded-lg border px-2 py-1 font-mono"
        style={fieldStyle}
        aria-label="Forge account"
      />
      <div className="text-ink-soft">Show it in</div>
      {rooms.length === 0 && <div className="text-ink-soft">Join or create a shared workspace first.</div>}
      {rooms.map((w) => (
        <label key={w.id} className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={picked.includes(w.id)}
            onChange={(e) => setPicked(e.target.checked ? [...picked, w.id] : picked.filter((x) => x !== w.id))}
          />
          {w.name}
        </label>
      ))}
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={discoverable} onChange={(e) => setDiscoverable(e.target.checked)} />
        Also put it on this device's contact card (people with your card see it)
      </label>
      <div className="text-ink-soft">How to prove it</div>
      <label className="flex items-center gap-2">
        <input type="radio" checked={method === "sshSig"} onChange={() => setMethod("sshSig")} />
        Sign with an SSH key the forge lists for me (recommended)
      </label>
      {forge.keyPublished && (
        <label className="flex items-start gap-2">
          <input type="radio" className="mt-0.5" checked={method === "keyPublished"} onChange={() => setMethod("keyPublished")} />
          <span>
            Or add my Ligus key to the forge as a signing key
            {method === "keyPublished" && <span className="mt-1 block" style={{ color: "var(--hive-danger-ink)" }}>{KEY_PUBLISHED_DISCLOSURE}</span>}
          </span>
        </label>
      )}
      <div className="flex gap-2 pt-1">
        <Button variant="primary" size="md" disabled={busy || !handle.trim() || (picked.length === 0 && !discoverable)} onClick={() => void prepare()}>
          {busy ? "Asking the forge…" : "Continue"}
        </Button>
        <Button size="md" onClick={onDone}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/// "Contact forges to check linked identities" (on by default) and the
/// self-managed forges this device may ask.
function ForgeCheckSettings() {
  const qc = useQueryClient();
  const s = useQuery({ queryKey: ["link-settings"], queryFn: identityLinkSettings });
  const [provider, setProvider] = useState("gitlab");
  const [url, setUrl] = useState("");

  async function save(next: { checkForges: boolean; forges: SelfHostedForge[] }) {
    try {
      await setIdentityLinkSettings(next);
      qc.invalidateQueries({ queryKey: ["link-settings"] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  if (!s.data) return null;
  const cur = s.data;
  return (
    <div className="space-y-2 pt-2 text-xs">
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={cur.checkForges} onChange={(e) => void save({ ...cur, checkForges: e.target.checked })} />
        Contact forges to check linked identities
      </label>
      <p className="text-ink-soft">
        Checking tells the forge (not your relay) that this device looked, at most once a day per link. With it off,
        links show "signature valid" and are never marked verified.
      </p>
      {cur.forges.map((f) => (
        <div key={`${f.provider}/${f.host}`} className="flex items-center justify-between gap-2">
          <span className="font-mono">
            {f.provider} · {f.baseUrl}
          </span>
          <button className="hover:opacity-80" style={{ color: "var(--hive-danger-ink)" }} onClick={() => void save({ ...cur, forges: cur.forges.filter((x) => x !== f) })}>
            Remove
          </button>
        </div>
      ))}
      <div className="flex gap-2">
        <select value={provider} onChange={(e) => setProvider(e.target.value)} className="rounded-lg border px-2 py-1" style={fieldStyle} aria-label="Self-managed forge kind">
          <option value="gitlab">GitLab</option>
          <option value="forgejo">Forgejo</option>
          <option value="gitea">Gitea</option>
          <option value="github">GitHub Enterprise</option>
        </select>
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://git.example.com"
          className="min-w-0 flex-1 rounded-lg border px-2 py-1 font-mono"
          style={fieldStyle}
          aria-label="Self-managed forge address"
        />
        <Button
          size="sm"
          disabled={!url.trim().startsWith("https://")}
          onClick={() => {
            void save({ ...cur, forges: [...cur.forges, { provider, host: "", baseUrl: url.trim() }] });
            setUrl("");
          }}
        >
          Add forge
        </Button>
      </div>
    </div>
  );
}
