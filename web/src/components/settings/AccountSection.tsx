import {
  DEVICES_REQUEST_EVENT,
  clearDevicesRequest,
  pendingDevicesRequest,
  type DevicesRequest,
} from "@/lib/nav";
import {
  cleanupWarning,
  groupDevices,
  loadHiddenRevoked,
  loadRevokedOpen,
  revokedOn,
  saveHiddenRevoked,
  saveRevokedOpen,
  unfinishedCleanup,
} from "@/lib/revokedDevices";
import { IconChevronDown } from "@/lib/icons";
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getAppSettings,
  setDisplayName,
  setAvatar,
  setGitEmail,
  myProfile,
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
  type DeviceRevocationDto,
  type MyDeviceDto,
  type RevocationStep,
} from "@/lib/ipc";
import { deviceName, pairingLine, revokeConfirmText } from "@/lib/myDevices";
import { confirmDialog } from "@/components/Dialog";
import { Button, Hint, InfoTip, ListRow, RowList, Section, Switch, fieldStyle } from "@/components/ui";
import { Row } from "@/components/settings/FlowSection";
import { toast, errMsg } from "@/components/Toast";
import { Avatar } from "@/components/Avatar";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { LinkedAccountsSection } from "@/components/settings/LinkedAccountsSection";
import { AddPhonePanel } from "@/components/settings/AddPhonePanel";
import { AddComputerPanel } from "@/components/settings/AddComputerPanel";
import { Sheet } from "@/components/kit/Sheet";
import { MoreMenu } from "@/components/MoreMenu";

/// Profile page: your name, picture, safety code and commit email, and the
/// devices that act as you. Your account is a key made on this device; no
/// provider signs you in. Changes apply immediately.
export function AccountSection() {
  return (
    <>
      <nav aria-label="On this page" className="mb-3 px-1 text-[11.5px] text-ink-soft">
        <a
          href="#my-devices"
          className="underline hover:text-ink"
          data-testid="jump-my-devices"
          onClick={(e) => {
            e.preventDefault();
            document.getElementById("my-devices")?.scrollIntoView?.({ block: "start" });
          }}
        >
          My devices
        </a>
      </nav>
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

/// Your devices in the active workspace: add a phone, or another computer of
/// yours (lt, the `ligus` CLI), so what it does is yours and not a separate
/// member's. Revoking a device makes every member stop accepting its events.
/// Revoked devices sit in a collapsed "Revoked" group at the bottom; one
/// whose cleanup isn't finished on some box stays in view with its retry.
export function DevicesSection() {
  const qc = useQueryClient();
  const devices = useQuery({ queryKey: ["my-devices"], queryFn: listMyDevices, refetchInterval: 15_000 });
  const requests = useQuery({ queryKey: ["device-requests"], queryFn: listDeviceRequests, refetchInterval: 15_000 });
  const revocations = useQuery({ queryKey: ["device-revocations"], queryFn: listDeviceRevocations, refetchInterval: 15_000 });
  const pairings = useQuery({ queryKey: ["device-pairings"], queryFn: listDevicePairings, refetchInterval: 30_000 });
  // Asked for from elsewhere (the palette, People, Set up): My devices, maybe
  // with Add a phone or Add a computer open.
  const initial = useRef<DevicesRequest | null>(pendingDevicesRequest());
  const [adding, setAdding] = useState(initial.current?.open === "computer");
  const [suggested, setSuggested] = useState<DevicesRequest["member"] | null>(initial.current?.member ?? null);
  const [addingPhone, setAddingPhone] = useState(initial.current?.open === "phone");
  const sectionRef = useRef<HTMLDivElement>(null);
  const [revokedOpen, setRevokedOpen] = useState(loadRevokedOpen);
  const [hidden, setHidden] = useState(loadHiddenRevoked);
  const [showHidden, setShowHidden] = useState(false);
  const [details, setDetails] = useState<Set<string>>(() => new Set());
  // Bring the section into view (it sits below the profile fields), then use
  // the request up. A section already on screen hears later requests.
  useEffect(() => {
    let t: number | undefined;
    const bring = () => {
      window.clearTimeout(t);
      // After the sheet and the sections above have laid out.
      t = window.setTimeout(() => sectionRef.current?.scrollIntoView?.({ block: "start" }), 200);
    };
    if (initial.current) {
      clearDevicesRequest();
      bring();
    }
    const onRequest = (e: Event) => {
      const req = (e as CustomEvent<DevicesRequest>).detail;
      clearDevicesRequest();
      if (req.open === "computer") {
        setSuggested(req.member ?? null);
        setAdding(true);
      } else if (req.open === "phone") setAddingPhone(true);
      bring();
    };
    window.addEventListener(DEVICES_REQUEST_EVENT, onRequest);
    return () => {
      window.clearTimeout(t);
      window.removeEventListener(DEVICES_REQUEST_EVENT, onRequest);
    };
  }, []);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["my-devices"] });
    qc.invalidateQueries({ queryKey: ["device-requests"] });
    qc.invalidateQueries({ queryKey: ["device-revocations"] });
    qc.invalidateQueries({ queryKey: ["device-pairings"] });
  };

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

  const toggleRevoked = () => {
    setRevokedOpen((v) => {
      saveRevokedOpen(!v);
      return !v;
    });
  };
  const setHiddenOne = (id: string, hide: boolean) => {
    setHidden((prev) => {
      const next = new Set(prev);
      if (hide) next.add(id);
      else next.delete(id);
      saveHiddenRevoked(next);
      return next;
    });
  };
  const toggleDetails = (id: string) =>
    setDetails((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const list = devices.data ?? [];
  const pending = requests.data ?? [];
  const reports = revocations.data ?? [];
  const pairs = pairings.data ?? [];
  const groups = groupDevices(list, reports, hidden);
  const revokedShown = showHidden ? [...groups.revoked, ...groups.hidden] : groups.revoked;
  const nameOf = (d: MyDeviceDto) => deviceName(d, pairs);
  const reportsOf = (d: MyDeviceDto) => reports.filter((r) => r.deviceId === d.deviceId);

  // The cleanup report (tunnel, push, session per box), behind Details.
  const detailsOf = (d: MyDeviceDto) => {
    if (!details.has(d.deviceId)) return null;
    const revs = reportsOf(d);
    return (
      <div className="px-1" data-testid={`device-details-${d.deviceId}`}>
        {revs.length === 0 ? (
          <div className="text-[11px] text-ink-soft">No box reports on it. Every member refuses its events.</div>
        ) : (
          revs.map((r) => <RevocationStatus key={r.hostId} r={r} onRetry={refresh} />)
        )}
      </div>
    );
  };
  const detailsAction = (d: MyDeviceDto) => ({
    label: details.has(d.deviceId) ? "Hide details" : "Details",
    onSelect: () => toggleDetails(d.deviceId),
    title: "What each box did: tunnel, push, session",
  });

  return (
    <Section
      id="my-devices"
      title="My devices"
      info="Devices that act as you in this workspace. Using Ligus on another computer? Add it here instead of inviting yourself, so it's you and not a second member."
      action={
        <div className="flex items-center gap-1.5">
          <Button
            size="sm"
            onClick={() => {
              setSuggested(null);
              setAdding(true);
            }}
          >
            Add a computer
          </Button>
          <MoreMenu label="More device actions" items={[{ label: "Add a phone…", onSelect: () => setAddingPhone(true) }]} />
        </div>
      }
    >
      <div ref={sectionRef} className="space-y-2.5">
        {pending.map((r) => (
          <div
            key={r.hostId}
            className="rounded-xl border px-3 py-2.5 text-xs"
            style={{ borderColor: "var(--ligus-accent-cool)" }}
          >
            <strong>{r.label}</strong> says it's your device, but boxes join as their own member now. On it, run{" "}
            <code>ligus admit request</code>, then approve it under <em>workspace menu → Admit a box</em>.
          </div>
        ))}
        {addingPhone && (
          <Sheet title="Add a phone" side="center" width={520} onClose={() => setAddingPhone(false)}>
            <AddPhonePanel
              onDone={() => {
                setAddingPhone(false);
                refresh();
              }}
              onCancel={() => setAddingPhone(false)}
            />
          </Sheet>
        )}
        {adding && (
          <Sheet title="Add a computer" side="center" width={520} onClose={() => setAdding(false)}>
            <AddComputerPanel
              suggested={suggested ?? undefined}
              onDone={() => {
                setAdding(false);
                refresh();
              }}
              onCancel={() => setAdding(false)}
            />
          </Sheet>
        )}
        <RowList label="My devices">
          {groups.active.map((d) => {
            const pl = pairingLine(d, pairs) ?? "";
            return (
              <ListRow
                key={d.deviceId}
                testId={`device-${d.deviceId}`}
                title={
                  <span>
                    {nameOf(d)}
                    {d.thisDevice && <span className="font-normal text-ink-soft"> · this device</span>}
                  </span>
                }
                meta={<span className="font-mono">{d.fingerprint}</span>}
                tip={pl || undefined}
                actionsLabel={`Actions for ${nameOf(d)}`}
                actions={!d.thisDevice ? [{ label: "Revoke", danger: true, onSelect: () => void revoke(d) }] : []}
              />
            );
          })}
          {groups.attention.map((d) => {
            const left = unfinishedCleanup(d.deviceId, reports);
            return (
              <ListRow
                key={d.deviceId}
                testId={`device-${d.deviceId}`}
                title={<span style={{ opacity: 0.7 }}>{nameOf(d)}</span>}
                meta={
                  <span style={{ color: "var(--ligus-warn-ink)" }} role="status">
                    revoked · {cleanupWarning(left[0])}
                  </span>
                }
                tip={d.fingerprint}
                trailing={<RetryButton deviceId={d.deviceId} hostLabel={left[0].hostLabel} onRetry={refresh} />}
                actionsLabel={`Actions for ${nameOf(d)}`}
                actions={[detailsAction(d)]}
              >
                {detailsOf(d)}
              </ListRow>
            );
          })}
        </RowList>
        {groups.revoked.length + groups.hidden.length > 0 && (
          <div data-testid="revoked-group">
            <div className="flex items-center justify-between gap-2 px-1">
              <button
                type="button"
                className="flex items-center gap-1 text-[11.5px] text-ink-soft hover:text-ink"
                aria-expanded={revokedOpen}
                onClick={toggleRevoked}
              >
                <span aria-hidden style={{ display: "inline-flex", transform: revokedOpen ? "none" : "rotate(-90deg)" }}>
                  <IconChevronDown size={12} />
                </span>
                Revoked ({groups.revoked.length})
              </button>
              {groups.hidden.length > 0 && (
                <MoreMenu
                  label="Revoked devices actions"
                  items={[
                    {
                      label: showHidden ? "Leave hidden ones out" : `Show hidden (${groups.hidden.length})`,
                      onSelect: () => {
                        setShowHidden((v) => !v);
                        if (!revokedOpen) toggleRevoked();
                      },
                    },
                  ]}
                />
              )}
            </div>
            {revokedOpen && (
              <RowList label="Revoked devices">
                {revokedShown.map((d) => {
                  const on = revokedOn(d.deviceId, reports);
                  const isHidden = hidden.has(d.deviceId);
                  return (
                    <ListRow
                      key={d.deviceId}
                      testId={`device-${d.deviceId}`}
                      title={<span style={{ opacity: 0.55 }}>{nameOf(d)}</span>}
                      meta={`revoked${on ? ` ${on}` : ""}${isHidden ? " · hidden" : ""}`}
                      tip={d.fingerprint}
                      actionsLabel={`Actions for ${nameOf(d)}`}
                      actions={[
                        detailsAction(d),
                        isHidden
                          ? { label: "Show in this list", onSelect: () => setHiddenOne(d.deviceId, false) }
                          : {
                              label: "Hide from this list",
                              onSelect: () => setHiddenOne(d.deviceId, true),
                              title: "Hides it here; it stays revoked",
                            },
                      ]}
                    >
                      {detailsOf(d)}
                    </ListRow>
                  );
                })}
              </RowList>
            )}
          </div>
        )}
      </div>
    </Section>
  );
}

/// The one visible action on a revoked device whose cleanup isn't finished.
function RetryButton({ deviceId, hostLabel, onRetry }: { deviceId: string; hostLabel: string; onRetry: () => void }) {
  const [busy, setBusy] = useState(false);
  async function retry() {
    setBusy(true);
    try {
      await retryDeviceRevocation(deviceId);
      toast.success(`Asked ${hostLabel} to retry.`);
      onRetry();
    } catch (e) {
      toast.error(`Couldn't ask for a retry: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Button size="sm" disabled={busy} onClick={() => void retry()}>
      Retry now
    </Button>
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
