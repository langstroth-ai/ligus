import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  syncStatus,
  probeRelay,
  type RelayProbeDto,
  getConnectionSettings,
  updateConnectionSettings,
  p2pMyCode,
  p2pListContacts,
  p2pAddContact,
  p2pRemoveContact,
  p2pShareCode,
  redeemShortCode,
  hasIssuerKey,
  setRelayIssuerKey,
} from "@/lib/ipc";
import {
  IconCheck,
  IconAlertTriangle,
  IconInfo,
  IconLock,
  IconHexagon,
  IconChevronDown,
  IconChevronRight,
} from "@/lib/icons";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { OrgSignInSection } from "./OrgSignInSection";

/// Team sync: the relay connection (E2EE), organization sign-in, and direct
/// peer (P2P) contacts. Connection fields write through on blur (D18).
export function TeamSyncSection() {
  return (
    <>
      <SyncSection />
      <OrgSignInSection />
      <PeersSection />
    </>
  );
}

const PROBE_COLOR: Record<RelayProbeDto["status"], string> = {
  ok: "var(--hive-success)",
  unauthorized: "var(--hive-warn)",
  httpError: "var(--hive-warn)",
  unreachable: "var(--hive-warn)",
  unconfigured: "var(--hive-ink)",
};
const PROBE_ICON: Record<RelayProbeDto["status"], ReactNode> = {
  ok: <IconCheck size={13} />,
  unauthorized: <IconAlertTriangle size={13} />,
  httpError: <IconAlertTriangle size={13} />,
  unreachable: <IconAlertTriangle size={13} />,
  unconfigured: <IconInfo size={13} />,
};

function SyncSection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const conn = useQuery({ queryKey: ["connection-settings"], queryFn: getConnectionSettings });
  const s = status.data;
  const c = conn.data;

  const [relayUrl, setRelayUrl] = useState("");
  const [room, setRoom] = useState("");
  const [workspaceKey, setWorkspaceKey] = useState("");
  const [relayAccessToken, setRelayAccessToken] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  // Relay issuer key — mints agent identity tokens for the "Add a remote agent"
  // generator (People pane). Local-only; only the public half sits on the relay.
  const [issuerKey, setIssuerKey] = useState("");
  const issuerSet = useQuery({ queryKey: ["has-issuer-key"], queryFn: hasIssuerKey });
  const saveIssuer = useMutation({
    mutationFn: (k: string) => setRelayIssuerKey(k),
    onSuccess: () => {
      setIssuerKey("");
      qc.invalidateQueries({ queryKey: ["has-issuer-key"] });
      toast.success("Issuer key saved.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const [probe, setProbe] = useState<RelayProbeDto | null>(null);

  // Live reachability + auth check (distinct from the config-only sync status).
  const testConnection = useMutation({
    mutationFn: probeRelay,
    onMutate: () => setProbe(null),
    onSuccess: (r) => setProbe(r),
    onError: (e) => setProbe({ status: "unreachable", detail: errMsg(e) }),
  });

  useEffect(() => {
    if (c) {
      setRelayUrl(c.relayUrl);
      setRoom(c.room);
      setWorkspaceKey("");
      setRelayAccessToken("");
    }
  }, [c]);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["sync-status"] });
    qc.invalidateQueries({ queryKey: ["connection-settings"] });
  };

  // Write the whole connection record through on any field blur. Blank secret
  // fields map to null (keep existing); permissionMode is preserved untouched —
  // it now lives on the Permissions page.
  async function commit() {
    try {
      await updateConnectionSettings({
        relayUrl,
        room,
        workspaceKey: workspaceKey === "" ? null : workspaceKey,
        apiKey: null,
        relayAccessToken: relayAccessToken === "" ? null : relayAccessToken,
        permissionMode: c?.permissionMode ?? "default",
      });
      setWorkspaceKey("");
      setRelayAccessToken("");
      refresh();
    } catch (e) {
      toast.error(`Couldn't save connection settings: ${errMsg(e)}`);
    }
  }

  // Clear a stored secret without touching the rest.
  const clearSecret = useMutation({
    mutationFn: (field: "workspaceKey" | "relayAccessToken") =>
      updateConnectionSettings({
        relayUrl,
        room,
        workspaceKey: field === "workspaceKey" ? "" : null,
        apiKey: null,
        relayAccessToken: field === "relayAccessToken" ? "" : null,
        permissionMode: c?.permissionMode ?? "default",
      }),
    onSuccess: refresh,
    onError: (e) => toast.error(`Couldn't update secret: ${errMsg(e)}`),
  });

  const inputClass = "w-full rounded-xl border px-3 py-2 font-mono text-sm";

  return (
    <Section title="Team sync">
      {/* Live status — driven by real connection health (connectionState), not
          just "a relay URL is set", so a dead relay / bad key is visible here. */}
      <div className="rounded-xl border px-3 py-2.5 text-sm" style={fieldStyle}>
        {s?.connectionState === "error" ? (
          <div className="space-y-1">
            <span className="flex items-center gap-1.5 font-medium" style={{ color: "var(--hive-danger-ink)" }}>
              <IconAlertTriangle size={13} /> Sync error
            </span>
            <div className="text-xs text-ink-soft">
              {s.lastError ?? "The last sync attempt failed."}
            </div>
            <div className="text-xs" style={{ color: "var(--hive-danger-ink)" }}>
              Reconnect — check your relay URL, key, or access token below, then test the connection.
            </div>
          </div>
        ) : s?.connectionState === "live" ? (
          <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
            <span className="flex items-center gap-1.5" style={{ color: "var(--hive-success-ink)" }}>
              <IconHexagon size={13} /> Live
            </span>
            <span className="text-ink-soft">·</span>
            <code className="text-xs">{s.relayUrl}</code>
            <span className="text-ink-soft">· room</span>
            <code className="text-xs">{s.room}</code>
            <span className="text-ink-soft">·</span>
            {s.encrypted ? (
              <span
                className="flex items-center gap-1"
                title="Messages sealed with the workspace key"
                style={{ color: "var(--hive-success-ink)" }}
              >
                <IconLock size={12} /> encrypted
              </span>
            ) : (
              <span className="flex items-center gap-1" style={{ color: "var(--hive-warn-ink)" }}>
                <IconAlertTriangle size={12} /> plaintext
              </span>
            )}
          </div>
        ) : s?.relayConfigured ? (
          <span className="text-ink-soft">Offline — a relay is configured but nothing is syncing right now.</span>
        ) : (
          <span className="text-ink-soft">Local only — not syncing with anyone yet.</span>
        )}
      </div>

      {/* Live reachability + auth — the config pill above only means a URL is
          set, so a real probe is the source of truth for "is it working". */}
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => testConnection.mutate()} disabled={testConnection.isPending}>
          {testConnection.isPending ? "Testing…" : "Test connection"}
        </Button>
        {probe ? (
          <span className="flex items-center gap-1.5 text-xs" style={{ color: PROBE_COLOR[probe.status] }}>
            {PROBE_ICON[probe.status]}
            {probe.detail}
          </span>
        ) : (
          // Surface the live sync failure right next to the fix affordance until
          // the user runs a fresh probe.
          s?.connectionState === "error" && (
            <span className="flex items-center gap-1.5 text-xs" style={{ color: "var(--hive-danger-ink)" }}>
              <IconAlertTriangle size={13} />
              {s.lastError ?? "Sync is failing — test the connection."}
            </span>
          )
        )}
      </div>

      {/* How teams work now */}
      <p className="text-xs text-ink-soft">
        Teams live in the left rail — hit <strong>＋</strong> to create or join one. Ligus
        generates the room, encryption key, and a shareable code for you. The relay below just
        brokers sync, short pairing codes, and member revocation; messages stay end-to-end
        encrypted and the relay only ever sees ciphertext.
      </p>

      <label className="block text-sm text-ink-soft">Relay URL</label>
      <input
        value={relayUrl}
        onChange={(e) => setRelayUrl(e.target.value)}
        onBlur={() => void commit()}
        placeholder="https://relay.example  ·  blank = local only"
        className={inputClass}
        style={fieldStyle}
      />
      <p className="text-xs text-ink-soft">Use the same relay URL on every device that should sync.</p>

      <label className="block text-sm text-ink-soft">Relay access token</label>
      <div className="flex items-center gap-2">
        <input
          type="password"
          value={relayAccessToken}
          onChange={(e) => setRelayAccessToken(e.target.value)}
          onBlur={() => void commit()}
          placeholder={c?.hasRelayAccessToken ? "configured — blank to keep" : "only for a relay that requires tokens"}
          className={"flex-1 " + inputClass}
          style={fieldStyle}
        />
        {c?.hasRelayAccessToken && (
          <button
            className="text-xs hover:opacity-80"
            style={{ color: "var(--hive-danger-ink)" }}
            onClick={() => clearSecret.mutate("relayAccessToken")}
          >
            clear
          </button>
        )}
      </div>
      <p className="text-xs text-ink-soft">
        Needed only for a relay that gates access with tokens. Leave blank for an open relay.
        Paste only the token value — if yours looks like <code>name:abc123</code>, drop the
        <code> name:</code> prefix.
      </p>

      {/* Advanced: manual room + key (the rail normally fills these in) */}
      <button
        type="button"
        className="flex items-center gap-1 text-xs opacity-60 hover:opacity-100"
        onClick={() => setShowAdvanced((v) => !v)}
      >
        {showAdvanced ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />} Advanced — set room &amp; key by hand
      </button>
      {showAdvanced && (
        <div className="space-y-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
          <p className="text-xs text-ink-soft">
            Normally the rail’s ＋ fills these in. Set them by hand only to join a specific room
            or change the passphrase directly.
          </p>
          <label className="block text-sm text-ink-soft">Room</label>
          <input
            value={room}
            onChange={(e) => setRoom(e.target.value)}
            onBlur={() => void commit()}
            placeholder="default"
            className={inputClass}
            style={fieldStyle}
          />
          <label className="block text-sm text-ink-soft">Workspace key (E2EE passphrase)</label>
          <div className="flex items-center gap-2">
            <input
              type="password"
              value={workspaceKey}
              onChange={(e) => setWorkspaceKey(e.target.value)}
              onBlur={() => void commit()}
              placeholder={c?.hasWorkspaceKey ? "configured — blank to keep" : "passphrase"}
              className={"flex-1 " + inputClass}
              style={fieldStyle}
            />
            {c?.hasWorkspaceKey && (
              <button
                className="text-xs hover:opacity-80"
                style={{ color: "var(--hive-danger-ink)" }}
                onClick={() => clearSecret.mutate("workspaceKey")}
              >
                clear
              </button>
            )}
          </div>

          <label className="block text-sm text-ink-soft">
            Relay issuer key {issuerSet.data ? "✓ set" : "(for adding remote agents)"}
          </label>
          <p className="text-xs text-ink-soft">
            The Ed25519 seed from <code>langstroth-relay keygen</code> (whose public half is on the relay).
            Lets you mint agent tokens locally for the People pane’s “Add a remote agent”. Stays on
            this device — never synced.
          </p>
          <div className="flex items-center gap-2">
            <input
              type="password"
              value={issuerKey}
              onChange={(e) => setIssuerKey(e.target.value)}
              placeholder={issuerSet.data ? "configured — enter to replace" : "64-hex Ed25519 seed"}
              className={"flex-1 " + inputClass}
              style={fieldStyle}
            />
            <Button size="sm" onClick={() => saveIssuer.mutate(issuerKey.trim())} disabled={!issuerKey.trim()}>
              Save
            </Button>
            {issuerSet.data && (
              <button
                className="text-xs hover:opacity-80"
                style={{ color: "var(--hive-danger-ink)" }}
                onClick={() => saveIssuer.mutate("")}
              >
                clear
              </button>
            )}
          </div>
        </div>
      )}
    </Section>
  );
}

function PeersSection() {
  const qc = useQueryClient();
  const myCode = useQuery({ queryKey: ["p2p-code"], queryFn: p2pMyCode, retry: false });
  const contacts = useQuery({ queryKey: ["p2p-contacts"], queryFn: p2pListContacts });
  const [code, setCode] = useState("");
  const [label, setLabel] = useState("");
  const [shortCode, setShortCode] = useState<{ code: string; expiresIn: number } | null>(null);
  const [redeemInput, setRedeemInput] = useState("");

  const share = useMutation({
    mutationFn: p2pShareCode,
    onSuccess: (r) => setShortCode(r),
    onError: (e) => toast.error(`Couldn't make a short code: ${errMsg(e)}`),
  });
  const redeem = useMutation({
    mutationFn: () => redeemShortCode(redeemInput.trim()),
    onSuccess: (r) => {
      setRedeemInput("");
      toast.success(r.kind === "workspace" ? `Joined "${r.label}".` : "Peer added.");
      qc.invalidateQueries({ queryKey: ["p2p-contacts"] });
      qc.invalidateQueries({ queryKey: ["workspaces"] });
    },
    onError: (e) => toast.error(`Couldn't use that code: ${errMsg(e)}`),
  });

  const add = useMutation({
    mutationFn: () => p2pAddContact(code.trim(), label.trim()),
    onSuccess: () => {
      setCode("");
      setLabel("");
      qc.invalidateQueries({ queryKey: ["p2p-contacts"] });
    },
    onError: (e) => toast.error(`Couldn't add peer: ${errMsg(e)}`),
  });
  const remove = useMutation({
    mutationFn: (id: string) => p2pRemoveContact(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["p2p-contacts"] }),
    onError: (e) => toast.error(`Couldn't remove peer: ${errMsg(e)}`),
  });

  return (
    <Section title="Direct peers (P2P)">
      <p className="text-xs text-ink-soft">
        Connect to a friend device-to-device (no shared room). Share your code; add theirs to sync
        directly. Falls back to a relay when a direct connection can't be made.
      </p>
      <label className="block text-sm text-ink-soft">Your peer code</label>
      <div className="flex gap-2">
        <input
          readOnly
          value={myCode.data ?? (myCode.isError ? "P2P unavailable in this build" : "…")}
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-xs"
          style={fieldStyle}
          onFocus={(e) => e.currentTarget.select()}
        />
        <Button
          variant="primary"
          size="md"
          disabled={!myCode.data}
          onClick={() => {
            if (myCode.data) {
              void navigator.clipboard.writeText(myCode.data);
              toast.success("Peer code copied.");
            }
          }}
        >
          Copy
        </Button>
        <Button
          size="md"
          disabled={!myCode.data || share.isPending}
          onClick={() => share.mutate()}
          title="Create a short, speakable code you can read out over a call"
        >
          {share.isPending ? "…" : "Short code"}
        </Button>
      </div>

      {shortCode && (
        <div className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2" style={{ borderColor: "var(--hive-accent-cool)" }}>
          <div>
            <div className="text-[10px] font-semibold uppercase tracking-wider text-ink-soft">
              Share this code (expires in {Math.round(shortCode.expiresIn / 60)} min)
            </div>
            <div className="font-mono text-2xl font-bold tracking-[0.2em]">{shortCode.code}</div>
          </div>
          <Button
            variant="primary"
            size="md"
            className="shrink-0"
            onClick={() => {
              void navigator.clipboard.writeText(shortCode.code);
              toast.success("Short code copied.");
            }}
          >
            Copy
          </Button>
        </div>
      )}

      <div className="flex gap-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <input
          value={redeemInput}
          onChange={(e) => setRedeemInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && redeemInput.trim() && redeem.mutate()}
          placeholder="Got a short code? e.g. K7P2QX"
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm uppercase"
          style={fieldStyle}
        />
        <Button
          variant="primary"
          size="md"
          className="shrink-0"
          disabled={!redeemInput.trim() || redeem.isPending}
          onClick={() => redeem.mutate()}
        >
          {redeem.isPending ? "…" : "Use code"}
        </Button>
      </div>

      <div className="space-y-2">
        {(contacts.data ?? []).map((c) => (
          <div key={c.peerId} className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2" style={fieldStyle}>
            <div className="min-w-0">
              <div className="font-medium">{c.label || "Unnamed peer"}</div>
              <div className="truncate font-mono text-xs text-ink-soft">{c.peerId}</div>
            </div>
            <button
              className="shrink-0 text-xs hover:opacity-80"
              style={{ color: "var(--hive-danger-ink)" }}
              onClick={() => confirmThen(`Remove peer "${c.label || c.peerId}"?`, () => remove.mutate(c.peerId))}
            >
              Remove
            </button>
          </div>
        ))}
        {(contacts.data ?? []).length === 0 && <p className="text-sm text-ink-soft">No peers added yet.</p>}
      </div>

      <div className="space-y-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <input
          value={code}
          onChange={(e) => setCode(e.target.value)}
          placeholder="Friend's full peer code (hive_…)"
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="Label (e.g. Sam's laptop)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
        <Button variant="primary" size="md" onClick={() => add.mutate()}>
          Add peer
        </Button>
      </div>
    </Section>
  );
}
