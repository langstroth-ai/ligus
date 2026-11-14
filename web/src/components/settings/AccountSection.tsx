import { useEffect, useState } from "react";
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
  type DeviceCodeDto,
} from "@/lib/ipc";
import { confirmDialog } from "@/components/Dialog";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { Avatar } from "@/components/Avatar";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { LinkedAccountsSection } from "@/components/settings/LinkedAccountsSection";

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
    </>
  );
}

/// Your safety code: the fingerprint of your account key, as eight groups of
/// four. People compare it out of band (in person, on a call) with the code
/// their invite or People list shows for you.
export function SafetyCodeSection() {
  const profile = useQuery({ queryKey: ["my-profile"], queryFn: myProfile });
  const code = profile.data?.safetyCode;
  return (
    <Section title="Safety code">
      <p className="text-xs text-ink-soft">
        Read this to someone you're inviting, or compare it with the code they see for you. If the codes match,
        the invite really is from you.
      </p>
      {code ? (
        <div className="flex items-center gap-3">
          <code className="font-mono text-sm font-bold tracking-wider" aria-label="Your safety code">
            {code}
          </code>
          <button
            className="text-xs underline opacity-70 hover:opacity-100"
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
        <p className="truncate font-mono text-[11px] text-ink-soft" title="Your account id: random, made on this device">
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
        <label className="group/av relative cursor-pointer" title="Upload an avatar">
          <Avatar name={settings.data?.displayName || name || "You"} url={settings.data?.avatarUrl} kind="human" size={52} />
          <span
            className="absolute inset-0 flex items-center justify-center rounded-full text-[10px] font-semibold opacity-0 transition-opacity group-hover/av:opacity-100"
            style={{ background: "color-mix(in srgb, var(--hive-ink) 45%, transparent)", color: "var(--hive-canvas)" }}
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
          <div>Your avatar is shown to everyone in the workspace.</div>
          {settings.data?.avatarUrl && (
            <button onClick={() => void clearAvatar()} className="mt-1 underline opacity-80 hover:opacity-100">
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
      <label className="mt-2 block text-sm text-ink-soft">Git email</label>
      <input
        value={email}
        onChange={(e) => setEmail(e.target.value)}
        onBlur={() => void commitEmail()}
        placeholder="you@example.com"
        className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
        style={fieldStyle}
      />
      <p className="text-xs text-ink-soft">
        Used to credit you as the commit author when an agent does work you asked for, even if it runs on a
        teammate's machine. Members of your workspaces can see it.
      </p>
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
  const [adding, setAdding] = useState(false);
  const [code, setCode] = useState("");
  const [preview, setPreview] = useState<DeviceCodeDto | null>(null);
  const [retire, setRetire] = useState(true);
  const [busy, setBusy] = useState(false);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["my-devices"] });
    qc.invalidateQueries({ queryKey: ["device-requests"] });
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

  async function revoke(deviceId: string, label: string) {
    const ok = await confirmDialog(
      `Revoke ${label}? Every member stops accepting its events right away. To use it again, run \`ligus serve enroll --new-device\` on it and approve the new code.`,
      { danger: true },
    );
    if (!ok) return;
    try {
      await revokeMyDevice(deviceId);
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
      action={
        !adding ? (
          <Button size="sm" onClick={() => setAdding(true)}>
            Add a device
          </Button>
        ) : undefined
      }
    >
      <p className="text-xs text-ink-soft">
        Devices that act as you in this workspace. Add your phone's home host (<code>ligus serve</code>) here
        so what you do from your phone is yours, not a separate member's.
      </p>
      {pending.map((r) => (
        <div
          key={r.hostId}
          className="rounded-xl border px-3 py-2.5 text-xs"
          style={{ borderColor: "var(--hive-accent-cool)" }}
        >
          <strong>{r.label}</strong> says it's your device. On it, run <code>ligus serve enroll</code> (or look in
          its log) and paste the code with <em>Add a device</em>.
        </div>
      ))}
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
                <div style={{ color: "var(--hive-danger-ink)" }}>This code is for a workspace this desktop doesn't have.</div>
              )}
              {preview.revoked && (
                <div style={{ color: "var(--hive-danger-ink)" }}>
                  This device was revoked. Run <code>ligus serve enroll --new-device</code> on it for a new code.
                </div>
              )}
              {preview.alreadyMine && <div className="opacity-70">Already your device.</div>}
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
              {d.label}
              {d.thisDevice && <span className="text-ink-soft"> · this device</span>}
              {d.revoked && <span style={{ color: "var(--hive-danger-ink)" }}> · revoked</span>}
            </div>
            <div className="truncate font-mono text-[11px] text-ink-soft">{d.fingerprint}</div>
          </div>
          {!d.thisDevice && !d.revoked && (
            <button
              className="shrink-0 text-xs hover:opacity-80"
              style={{ color: "var(--hive-danger-ink)" }}
              onClick={() => void revoke(d.deviceId, d.label)}
            >
              Revoke
            </button>
          )}
        </div>
      ))}
    </Section>
  );
}
