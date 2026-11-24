import { addPhoneRequested, clearAddPhoneRequest } from "@/lib/nav";
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getAppSettings,
  setDisplayName,
  setAvatar,
  setGitEmail,
  myProfile,
  inspectDeviceCode,
  approveDevice,
  listMyDevices,
  listDeviceRequests,
  revokeMyDevice,
  listDeviceRevocations,
  listDevicePairings,
  retryDeviceRevocation,
  getShareTyping,
  setShareTyping,
  getPrimaryOpenToMembers,
  setPrimaryOpenToMembers,
  type DeviceCodeDto,
  type DeviceRevocationDto,
  type MyDeviceDto,
  type RevocationStep,
} from "@/lib/ipc";
import { deviceName, pairingLine, revokeConfirmText } from "@/lib/myDevices";
import { confirmDialog } from "@/components/Dialog";
import { Button, Hint, InfoTip, Section, Switch, fieldStyle } from "@/components/ui";
import { Row } from "@/components/settings/FlowSection";
import { toast, errMsg } from "@/components/Toast";
import { Avatar } from "@/components/Avatar";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { LinkedAccountsSection } from "@/components/settings/LinkedAccountsSection";
import { AddPhonePanel } from "@/components/settings/AddPhonePanel";

/// Profile page: your name, picture, safety code and commit email, and the
/// devices that act as you. Your account is a key made on this device; no
/// provider signs you in. Changes apply immediately.
export function AccountSection() {
  return (
    <>
      <IdentitySection />
      <SafetyCodeSection />
      <LinkedAccountsSection />
      <DevicesSection />
      <PrivacySection />
    </>
  );
}

/// What this device tells others about you while you're in a chat.
export function PrivacySection() {
  const qc = useQueryClient();
  const share = useQuery({ queryKey: ["share-typing"], queryFn: getShareTyping });
  const primary = useQuery({ queryKey: ["primary-open"], queryFn: getPrimaryOpenToMembers });
  return (
    <Section title="Privacy">
      <Row
        label="Share typing status"
        hint="Show people in a chat when you're typing. Only that you're typing is shared, encrypted for the workspace; never what you type."
      >
        <Switch
          on={share.data ?? true}
          disabled={share.isLoading}
          label="Share typing status"
          onChange={(v) => {
            qc.setQueryData(["share-typing"], v);
            void setShareTyping(v)
              .catch((e) => toast.error(errMsg(e)))
              .finally(() => void qc.invalidateQueries({ queryKey: ["share-typing"] }));
          }}
        />
      </Row>
      <Row
        label="Let members use @ligus on this device"
        hint="In a chat with other people, @ligus runs on this device and your account. Off: it answers only you, and other members are told to use their own agents."
        note="Runs on this device and your account."
      >
        <Switch
          on={primary.data ?? false}
          disabled={primary.isLoading}
          label="Let members use @ligus on this device"
          onChange={(v) => {
            qc.setQueryData(["primary-open"], v);
            void setPrimaryOpenToMembers(v)
              .catch((e) => toast.error(errMsg(e)))
              .finally(() => void qc.invalidateQueries({ queryKey: ["primary-open"] }));
          }}
        />
      </Row>
    </Section>
  );
}

/// Your safety code: the fingerprint of your account key, as eight groups of
/// four. People compare it out of band (in person, on a call) with the code
/// their invite or People list shows for you.
export function SafetyCodeSection() {
  const profile = useQuery({ queryKey: ["my-profile"], queryFn: myProfile });
  const code = profile.data?.safetyCode;
  return (
    <Section
      title="Safety code"
      info="Read this to someone you're inviting, or compare it with the code they see for you. If the codes match, the invite really is from you."
    >
      <Hint>Compare it with the code they see for you.</Hint>
      {code ? (
        <div className="flex items-center gap-3">
          <code className="font-mono text-sm font-bold tracking-wider" aria-label="Your safety code">
            {code}
          </code>
          <button
            className="text-xs underline text-ink-soft hover:text-ink"
            onClick={() => {
              void navigator.clipboard.writeText(code);
              toast.success("Safety code copied.");
            }}
          >
            copy
          </button>
        </div>
      ) : (
        <p className="text-xs text-ink-soft">
          {profile.isLoading ? "…" : "This device acts for an account whose key it doesn't hold; see the code on your main device."}
        </p>
      )}
      {profile.data?.accountId && (
        <p className="truncate font-mono text-[11px] text-ink-soft" data-tip="Your account id: random, made on this device">
          Account {profile.data.accountId}
        </p>
      )}
    </Section>
  );
}

// Identity writes through on blur — no Save button (D18). A keystroke only
// re-renders this section, not the query-bearing siblings.
function IdentitySection() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");

  useEffect(() => {
    if (settings.data) {
      setName(settings.data.displayName);
      setEmail(settings.data.gitEmail);
    }
  }, [settings.data]);

  async function commitName() {
    const next = name.trim() || "You";
    if (next === (settings.data?.displayName ?? "")) return;
    try {
      await setDisplayName(next);
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["my-profile"] });
    } catch (e) {
      toast.error(`Couldn't save display name: ${errMsg(e)}`);
    }
  }

  async function commitEmail() {
    const next = email.trim();
    if (next === (settings.data?.gitEmail ?? "")) return;
    try {
      await setGitEmail(next);
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["my-profile"] });
    } catch (e) {
      toast.error(`Couldn't save git email: ${errMsg(e)}`);
    }
  }

  async function pickAvatar(file: File | undefined) {
    if (!file) return;
    try {
      const url = await fileToAvatarDataUrl(file);
      await setAvatar(url);
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["my-profile"] });
    } catch (e) {
      toast.error(`Couldn't set avatar: ${errMsg(e)}`);
    }
  }
  async function clearAvatar() {
    try {
      await setAvatar(null);
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["my-profile"] });
    } catch (e) {
      toast.error(`Couldn't clear avatar: ${errMsg(e)}`);
    }
  }

  return (
    <Section title="Profile">
      <div className="mb-4 flex items-center gap-3">
        <label className="group/av relative cursor-pointer" data-tip="Upload an avatar. Everyone in the workspace sees it.">
          <Avatar name={settings.data?.displayName || name || "You"} url={settings.data?.avatarUrl} kind="human" size={52} />
          <span
            className="absolute inset-0 flex items-center justify-center rounded-full text-[11px] font-semibold opacity-0 transition-opacity group-hover/av:opacity-100"
            style={{ background: "color-mix(in srgb, var(--ligus-ink) 45%, transparent)", color: "var(--ligus-canvas)" }}
          >
            Edit
          </span>
          <input
            type="file"
            accept="image/*"
            className="hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              e.target.value = "";
              void pickAvatar(f);
            }}
          />
        </label>
        <div className="text-xs text-ink-soft">
          {settings.data?.avatarUrl && (
            <button onClick={() => void clearAvatar()} className="mt-1 underline text-ink-soft hover:text-ink">
              Remove
            </button>
          )}
        </div>
      </div>
      <label className="block text-sm text-ink-soft">Display name</label>
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        onBlur={() => void commitName()}
        className="w-full rounded-xl border px-3 py-2 text-sm"
        style={fieldStyle}
      />
      <div className="mt-2 flex items-center gap-1.5">
        <label className="block text-sm text-ink-soft" htmlFor="profile-git-email">
          Git email
        </label>
        <InfoTip
          label="Git email"
          text="Used to credit you as the commit author when an agent does work you asked for, even if it runs on a teammate's machine. Members of your workspaces can see it."
        />
      </div>
      <input
        value={email}
        onChange={(e) => setEmail(e.target.value)}
        onBlur={() => void commitEmail()}
        id="profile-git-email"
        placeholder="you@example.com"
        className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
        style={fieldStyle}
      />
      <Hint>Visible to workspace members.</Hint>
      <span className="text-xs text-ink-soft">Device: {settings.data?.deviceName ?? "…"}</span>
    </Section>
  );
}

/// Your devices in the active workspace. A `ligus serve` box (the home host your
/// phone talks through) becomes one of them when you approve its enrollment
/// code here; from then on everything it does is authored as you. Revoking a
/// device makes every member stop accepting its events.
export function DevicesSection() {
  const qc = useQueryClient();
  const devices = useQuery({ queryKey: ["my-devices"], queryFn: listMyDevices, refetchInterval: 15_000 });
  const requests = useQuery({ queryKey: ["device-requests"], queryFn: listDeviceRequests, refetchInterval: 15_000 });
  const revocations = useQuery({ queryKey: ["device-revocations"], queryFn: listDeviceRevocations, refetchInterval: 15_000 });
  const pairings = useQuery({ queryKey: ["device-pairings"], queryFn: listDevicePairings, refetchInterval: 30_000 });
  const [adding, setAdding] = useState(false);
  // Opened straight from the palette's "Add a phone".
  const [addingPhone, setAddingPhone] = useState(addPhoneRequested);
  const phoneRef = useRef<HTMLDivElement>(null);
  const phoneRequested = useRef(addingPhone);
  // Asked for from the palette: bring the panel into view (it sits below the
  // profile fields), then use the request up.
  useEffect(() => {
    if (!phoneRequested.current) return;
    clearAddPhoneRequest();
    // After the sheet and the sections above have laid out.
    const t = window.setTimeout(() => phoneRef.current?.scrollIntoView?.({ block: "start" }), 200);
    return () => window.clearTimeout(t);
  }, []);
  const [code, setCode] = useState("");
  const [preview, setPreview] = useState<DeviceCodeDto | null>(null);
  const [retire, setRetire] = useState(true);
  const [busy, setBusy] = useState(false);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["my-devices"] });
    qc.invalidateQueries({ queryKey: ["device-requests"] });
    qc.invalidateQueries({ queryKey: ["device-revocations"] });
    qc.invalidateQueries({ queryKey: ["device-pairings"] });
  };

  async function check(next: string) {
    setCode(next);
    setPreview(null);
    if (!next.trim()) return;
    try {
      setPreview(await inspectDeviceCode(next));
    } catch (e) {
      if (next.trim().startsWith("hivedev1:")) toast.error(errMsg(e));
    }
  }

  async function approve() {
    if (!preview) return;
    setBusy(true);
    try {
      const r = await approveDevice(code, retire && Boolean(preview.replacesMember));
      toast.success(
        r.retired
          ? `${preview.label} is now your device; its separate member "${r.retired}" was removed.`
          : `${preview.label} is now your device.`,
      );
      if (r.retireError) toast.error(`Couldn't remove its separate member: ${r.retireError}`);
      setAdding(false);
      setCode("");
      setPreview(null);
      refresh();
    } catch (e) {
      toast.error(`Couldn't approve the device: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function revoke(d: MyDeviceDto) {
    const label = deviceName(d, pairings.data ?? []);
    const ok = await confirmDialog(revokeConfirmText(d, pairings.data ?? []), { danger: true });
    if (!ok) return;
    try {
      await revokeMyDevice(d.deviceId);
      toast.success(`${label} was revoked.`);
      refresh();
    } catch (e) {
      toast.error(`Couldn't revoke: ${errMsg(e)}`);
    }
  }

  const list = devices.data ?? [];
  const pending = requests.data ?? [];

  return (
    <Section
      title="My devices"
      info="Devices that act as you in this workspace. Add your phone's home host (ligus serve) here so what you do from your phone is yours, not a separate member's."
      action={
        !adding && !addingPhone ? (
          <div className="flex gap-2">
            <Button size="sm" onClick={() => setAddingPhone(true)}>
              Add a phone
            </Button>
            <Button size="sm" onClick={() => setAdding(true)}>
              Add a device
            </Button>
          </div>
        ) : undefined
      }
    >
      {pending.map((r) => (
        <div
          key={r.hostId}
          className="rounded-xl border px-3 py-2.5 text-xs"
          style={{ borderColor: "var(--ligus-accent-cool)" }}
        >
          <strong>{r.label}</strong> says it's your device. On it, run <code>ligus serve enroll</code> (or look in
          its log) and paste the code with <em>Add a device</em>.
        </div>
      ))}
      {addingPhone && (
        <div ref={phoneRef}>
          <AddPhonePanel
            onDone={() => {
              setAddingPhone(false);
              refresh();
            }}
            onCancel={() => setAddingPhone(false)}
          />
        </div>
      )}
      {adding && (
        <div className="space-y-2 rounded-xl border px-3 py-3" style={fieldStyle}>
          <textarea
            value={code}
            onChange={(e) => void check(e.target.value)}
            placeholder="Paste the code from the device: hivedev1:…"
            rows={3}
            className="w-full rounded-xl border px-3 py-2 font-mono text-xs"
            style={fieldStyle}
            aria-label="Device enrollment code"
          />
          {preview && (
            <div className="space-y-1.5 text-xs">
              <div>
                <span className="text-ink-soft">Device</span> <strong>{preview.label || "unnamed"}</strong>
              </div>
              <div>
                <span className="text-ink-soft">Fingerprint</span>{" "}
                <code className="font-bold tracking-wider">{preview.fingerprint}</code>
                <span className="text-ink-soft"> (must match the device's terminal)</span>
              </div>
              {!preview.knownWorkspace && (
                <div style={{ color: "var(--ligus-danger-ink)" }}>This code is for a workspace this desktop doesn't have.</div>
              )}
              {preview.revoked && (
                <div style={{ color: "var(--ligus-danger-ink)" }}>
                  This device was revoked. Run <code>ligus serve enroll --new-device</code> on it for a new code.
                </div>
              )}
              {preview.alreadyMine && <div className="text-ink-soft">Already your device.</div>}
              {preview.replacesMember && (
                <label className="flex items-center gap-2">
                  <input type="checkbox" checked={retire} onChange={(e) => setRetire(e.target.checked)} />
                  Remove its separate member "{preview.replacesMember.displayName}" (its past messages show as yours,
                  via the device)
                </label>
              )}
            </div>
          )}
          <div className="flex gap-2">
            <Button
              variant="primary"
              size="md"
              disabled={!preview || busy || preview.revoked || !preview.knownWorkspace || preview.alreadyMine}
              onClick={() => void approve()}
            >
              Approve as my device
            </Button>
            <Button
              size="md"
              onClick={() => {
                setAdding(false);
                setCode("");
                setPreview(null);
              }}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}
      {list.map((d) => (
        <div
          key={d.deviceId}
          className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2"
          style={{ ...fieldStyle, opacity: d.revoked ? 0.55 : 1 }}
        >
          <div className="min-w-0">
            <div className="truncate text-sm font-medium">
              {deviceName(d, pairings.data ?? [])}
              {d.thisDevice && <span className="text-ink-soft"> · this device</span>}
              {d.revoked && <span style={{ color: "var(--ligus-danger-ink)" }}> · revoked</span>}
            </div>
            <div className="truncate font-mono text-[11px] text-ink-soft">{d.fingerprint}</div>
            {!d.revoked && pairingLine(d, pairings.data ?? []) && (
              <div className="truncate text-[11px] text-ink-soft">{pairingLine(d, pairings.data ?? [])}</div>
            )}
            {d.revoked &&
              (revocations.data ?? [])
                .filter((r) => r.deviceId === d.deviceId)
                .map((r) => <RevocationStatus key={r.hostId} r={r} onRetry={refresh} />)}
          </div>
          {!d.thisDevice && !d.revoked && (
            <button
              className="shrink-0 text-xs hover:opacity-80"
              style={{ color: "var(--ligus-danger-ink)" }}
              onClick={() => void revoke(d)}
            >
              Revoke
            </button>
          )}
        </div>
      ))}
    </Section>
  );
}

const STEP_LABEL: Record<RevocationStep["state"], string> = {
  done: "done",
  pending: "pending",
  failed: "failed, retrying",
  not_applicable: "not applicable",
};

/// One box's report on a revoked phone: tunnel, push and session, each with
/// its state and why, plus "retry now" while anything is unfinished.
function RevocationStatus({ r, onRetry }: { r: DeviceRevocationDto; onRetry: () => void }) {
  const [busy, setBusy] = useState(false);
  const steps: [string, RevocationStep][] = [
    ["Tunnel", r.tunnel],
    ["Push", r.push],
    ["Session", r.session],
  ];
  async function retry() {
    setBusy(true);
    try {
      await retryDeviceRevocation(r.deviceId);
      toast.success(`Asked ${r.hostLabel} to retry.`);
      onRetry();
    } catch (e) {
      toast.error(`Couldn't ask for a retry: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="mt-1 text-[11px] text-ink-soft">
      <div>
        {r.hostLabel}: {r.summary}
      </div>
      <ul className="ml-3">
        {steps.map(([label, s]) => (
          <li key={label} data-tip={s.detail}>
            <span style={{ color: s.state === "failed" ? "var(--ligus-danger-ink)" : undefined }}>
              {label}: {STEP_LABEL[s.state]}
            </span>
            {s.detail ? <span> · {s.detail}</span> : null}
          </li>
        ))}
      </ul>
      {!r.complete && (
        <button className="text-xs hover:opacity-80" disabled={busy} onClick={() => void retry()}>
          Retry now
        </button>
      )}
    </div>
  );
}
