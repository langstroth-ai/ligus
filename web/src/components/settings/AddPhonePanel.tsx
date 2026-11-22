import { useCallback, useEffect, useRef, useState } from "react";
import {
  certifyPhoneCode,
  inspectPhoneCode,
  phoneEnrollOfferApprove,
  phoneEnrollOfferCancel,
  phoneEnrollOfferDecline,
  phoneEnrollOfferHide,
  phoneEnrollOfferPoll,
  phoneEnrollOfferShow,
  phoneEnrollOfferStart,
  phoneEnrollPrecheck,
  type EnrollAccountDto,
  type EnrollPhoneDto,
  type PhoneCodeDto,
  type PhoneEnrollOfferDto,
  type PhoneEnrollPrecheckDto,
} from "@/lib/ipc";
import { Button, LinkButton, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// How often the panel asks the app for the phone's request.
export const POLL_MS = 2_000;
/// The countdown and the hold tick this often.
const TICK_MS = 500;

type Phase =
  | "checking" // the precheck runs
  | "idle" // Show code not pressed yet
  | "shown" // the QR is on screen
  | "hidden" // the window lost focus: the offer stays, the QR is gone
  | "request" // a phone answered
  | "approving" // the passkey sheet is up
  | "aborted" // two phones answered
  | "expired";

/// "Add a phone" (Settings → Profile → My devices). The desktop shows a QR
/// code; the phone scans it with the Ligus app (Settings → This phone → Add
/// to my devices) and its request comes back through your box. Compare the
/// fingerprint with the phone's screen and confirm with your passkey: the
/// phone becomes one of your devices at the phone tier (it can post, vote and
/// answer as you, never change members, devices or keys). **Can't scan?**
/// opens the paste flow.
///
/// The QR is drawn by the app (the page never holds the code as text), only
/// after Show code, hidden when the window loses focus, and the window is
/// kept out of screenshots and most screen sharing while it shows.
export function AddPhonePanel({ onDone, onCancel }: { onDone: () => void; onCancel: () => void }) {
  const [phase, setPhase] = useState<Phase>("checking");
  const [precheck, setPrecheck] = useState<PhoneEnrollPrecheckDto | null>(null);
  const [offer, setOffer] = useState<{ rid: string; expiresAt: number; account: EnrollAccountDto } | null>(null);
  const [qrSvg, setQrSvg] = useState<string | null>(null);
  const [phone, setPhone] = useState<EnrollPhoneDto | null>(null);
  const [holdUntil, setHoldUntil] = useState(0);
  const [matches, setMatches] = useState(false);
  const [paste, setPaste] = useState(false);
  const [announce, setAnnounce] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const phaseRef = useRef(phase);
  phaseRef.current = phase;
  const ridRef = useRef<string | null>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const announced60 = useRef(false);

  useEffect(() => {
    let alive = true;
    phoneEnrollPrecheck()
      .then((p) => {
        if (!alive) return;
        setPrecheck(p);
        setPaste(!p.ready);
        setPhase("idle");
      })
      .catch(() => {
        if (!alive) return;
        setPrecheck({ ready: false, missing: [] });
        setPaste(true);
        setPhase("idle");
      });
    return () => {
      alive = false;
    };
  }, []);

  // Closing the panel drops an offer nobody answered yet (an approved one is
  // already watched in the background).
  useEffect(
    () => () => {
      if (ridRef.current) void phoneEnrollOfferCancel().catch(() => {});
    },
    [],
  );

  const take = (o: PhoneEnrollOfferDto) => {
    ridRef.current = o.rid;
    announced60.current = false;
    setOffer({ rid: o.rid, expiresAt: o.expiresAt, account: o.account });
    setQrSvg(o.qrSvg);
    setPhone(null);
    setMatches(false);
    setAnnounce("");
    setNow(Date.now());
    setPhase("shown");
  };

  async function showCode() {
    const again = phase === "hidden" && ridRef.current;
    try {
      // The same offer again after a blur; a new one otherwise.
      take(again ? await phoneEnrollOfferShow() : await phoneEnrollOfferStart());
    } catch (e) {
      if (again) {
        // The offer closed meanwhile (it expired): draw a new one.
        try {
          take(await phoneEnrollOfferStart());
          return;
        } catch (e2) {
          toast.error(`Couldn't show a code: ${errMsg(e2)}`);
          return;
        }
      }
      toast.error(`Couldn't show a code: ${errMsg(e)}`);
    }
  }

  const hideQr = useCallback(() => {
    if (phaseRef.current !== "shown") return;
    setQrSvg(null);
    setPhase("hidden");
    void phoneEnrollOfferHide().catch(() => {});
  }, []);

  // Hidden when the window loses focus or the app goes to the background.
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState === "hidden") hideQr();
    };
    window.addEventListener("blur", hideQr);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("blur", hideQr);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [hideQr]);

  // The countdown and the hold.
  const live = phase === "shown" || phase === "hidden" || phase === "request" || phase === "approving";
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(t);
  }, [live]);
  const remainingS = offer ? Math.max(0, Math.ceil((offer.expiresAt * 1000 - now) / 1000)) : 0;
  useEffect(() => {
    if (!offer || !live) return;
    if (remainingS <= 60 && remainingS > 0 && !announced60.current) {
      announced60.current = true;
      setAnnounce("One minute left to scan the code.");
    }
  }, [remainingS, offer, live]);

  // Poll for the phone's request, one call at a time.
  useEffect(() => {
    if (!live) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const got = await phoneEnrollOfferPoll();
        if (!alive) return;
        if (got.state === "request" && got.phone && got.rid === ridRef.current) {
          const p = got.phone;
          setHoldUntil(Date.now() + (got.holdRemainingMs ?? 0));
          setPhone((prev) => {
            if (!prev) setTimeout(() => cardRef.current?.focus(), 0);
            return p;
          });
          if (phaseRef.current === "shown" || phaseRef.current === "hidden") {
            // The QR has done its job: take it off the screen.
            setQrSvg(null);
            void phoneEnrollOfferHide().catch(() => {});
            setPhase("request");
          }
        } else if (got.state === "aborted") {
          setAnnounce("Two phones answered this code.");
          if (phaseRef.current !== "approving") {
            ridRef.current = null;
            setQrSvg(null);
            setPhone(null);
            setPhase("aborted");
            return;
          }
        } else if (got.state === "expired" || got.state === "none") {
          if (phaseRef.current !== "approving") {
            ridRef.current = null;
            setQrSvg(null);
            setPhone(null);
            setAnnounce("The code expired.");
            setPhase("expired");
            return;
          }
        }
      } catch {
        // A relay hiccup: try again on the next tick.
      }
      if (alive) timer = setTimeout(() => void tick(), POLL_MS);
    };
    timer = setTimeout(() => void tick(), POLL_MS);
    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
  }, [live]);

  async function approve() {
    if (!phone || !offer) return;
    setPhase("approving");
    try {
      const r = await phoneEnrollOfferApprove(offer.rid, phone.fingerprint);
      ridRef.current = null;
      const until = new Date(r.watchingUntil * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
      toast.success(
        `${r.label || "Your phone"} is now one of your devices; the phone finishes on its own. Until ${until}, Ligus warns you if another phone answers the same code.`,
      );
      onDone();
    } catch (e) {
      toast.error(`Couldn't add the phone: ${errMsg(e)}`);
      setPhase("request");
    }
  }

  async function decline() {
    if (!offer) return;
    try {
      await phoneEnrollOfferDecline(offer.rid);
    } catch (e) {
      toast.error(errMsg(e));
    }
    ridRef.current = null;
    setOffer(null);
    setPhone(null);
    setQrSvg(null);
    setPhase("idle");
  }

  function close() {
    if (ridRef.current) {
      ridRef.current = null;
      void phoneEnrollOfferCancel().catch(() => {});
    }
    onCancel();
  }

  const canAdd = phone !== null && (phone.standing === "new" || phone.standing === "alreadyMine");
  const holding = now < holdUntil;
  const holdS = Math.max(1, Math.ceil((holdUntil - now) / 1000));
  const unavailable = precheck !== null && !precheck.ready;

  return (
    <div className="space-y-3 rounded-xl border px-3 py-3" style={fieldStyle} data-testid="add-phone">
      <div aria-live="polite" className="sr-only" data-testid="add-phone-announce">
        {announce}
      </div>
      {phase === "checking" && <p className="text-xs text-ink-soft">Checking this desktop…</p>}
      {unavailable && (
        <div className="space-y-1 text-xs" data-testid="add-phone-unavailable">
          <p>Adding a phone by scanning a code isn't available here right now:</p>
          <ul className="list-disc pl-4 text-ink-soft">
            {precheck.missing.length ? (
              precheck.missing.map((m) => <li key={m}>{m}</li>)
            ) : (
              <li>The app couldn't check this desktop.</li>
            )}
          </ul>
          <p className="text-ink-soft">You can still add it with the phone's code below.</p>
        </div>
      )}

      {!unavailable && phase === "idle" && (
        <div className="space-y-2">
          <p className="text-xs text-ink-soft">
            Show a code here, then scan it with the Ligus app on your phone. Your phone needs to reach a box of this
            workspace (its pairing link).
          </p>
          <div className="flex gap-2">
            <Button variant="primary" size="md" onClick={() => void showCode()}>
              Show code
            </Button>
            <Button size="md" onClick={close}>
              Cancel
            </Button>
          </div>
        </div>
      )}

      {(phase === "shown" || phase === "hidden") && offer && (
        <div className="space-y-2">
          {phase === "shown" && qrSvg ? (
            <div
              role="img"
              aria-label="QR code to add a phone. Can't scan? Use the paste option below."
              data-testid="add-phone-qr"
              className="mx-auto w-[220px] max-w-full select-none rounded-lg bg-white p-1"
              style={{ userSelect: "none", WebkitUserSelect: "none" }}
              onContextMenu={(e) => e.preventDefault()}
              onCopy={(e) => e.preventDefault()}
              onDragStart={(e) => e.preventDefault()}
              // Drawn by the app from the offer; it carries no page input.
              dangerouslySetInnerHTML={{ __html: qrSvg }}
            />
          ) : (
            <div className="space-y-2 text-xs" data-testid="add-phone-hidden">
              <p className="text-ink-soft">The code is hidden while Ligus isn't in front. It's still valid.</p>
              <Button variant="primary" size="md" onClick={() => void showCode()}>
                Show code
              </Button>
            </div>
          )}
          <p className="text-xs">
            Scan with the Ligus app on your phone: <em>Settings → This phone → Add to my devices</em>.
          </p>
          <p className="text-xs text-ink-soft" aria-hidden data-testid="add-phone-countdown">
            Expires in {Math.floor(remainingS / 60)}:{String(remainingS % 60).padStart(2, "0")}
          </p>
          <p className="sr-only" data-testid="add-phone-expiry-text">
            The code expires at{" "}
            {new Date(offer.expiresAt * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}.
          </p>
          <Button size="md" onClick={close}>
            Cancel
          </Button>
        </div>
      )}

      {(phase === "request" || phase === "approving") && phone && offer && (
        <div
          ref={cardRef}
          tabIndex={-1}
          role="group"
          aria-label={`${phone.label || "A phone"} wants to be added`}
          className="space-y-1.5 text-xs outline-none"
          data-testid="add-phone-request"
        >
          <div className="text-sm">
            <strong>{phone.label || "An unnamed phone"}</strong> wants to be added
          </div>
          <div className="text-ink-soft" data-testid="phone-name-hint">
            (the name the phone gives itself; the fingerprint is what proves it's yours)
          </div>
          <div className="text-ink-soft">Relayed by {phone.via}</div>
          <div data-testid="add-phone-account">
            It will act as you,{" "}
            <strong>
              {offer.account.displayName || "you"}
              {offer.account.role ? ` (${offer.account.role})` : ""}
            </strong>
            , in <strong>{offer.account.workspaceName}</strong>, at the phone tier.
          </div>
          {offer.account.nameCollision && (
            <div style={{ color: "var(--ligus-danger-ink)" }} data-testid="add-phone-name-collision">
              Someone else here is also called {offer.account.displayName}. The phone warns about it too: check that it
              names this workspace and your role.
            </div>
          )}
          {phone.duplicateLabel && (
            <div style={{ color: "var(--ligus-danger-ink)" }} data-testid="add-phone-duplicate">
              You already have a device called {phone.label}. Make sure this is the phone in your hand.
            </div>
          )}
          <div>
            <span className="text-ink-soft">Fingerprint</span>{" "}
            <code
              className="font-bold tracking-wider"
              data-testid="phone-fingerprint"
              aria-label={spellFingerprint(phone.fingerprint)}
            >
              {phone.fingerprint}
            </code>
          </div>
          {phone.standing === "alreadyMine" && (
            <div className="text-ink-soft" data-testid="phone-already-mine">
              Already one of your devices. Adding it again keeps it and updates its name in My devices.
            </div>
          )}
          {phone.problem && <div style={{ color: "var(--ligus-danger-ink)" }}>{phone.problem}</div>}
          {canAdd && (
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={matches} onChange={(e) => setMatches(e.target.checked)} />
              Your phone shows the same fingerprint
            </label>
          )}
          {canAdd && holding && (
            <div className="text-ink-soft" data-testid="add-phone-hold">
              Listening for other phones… {holdS} s
            </div>
          )}
          <div className="flex gap-2 pt-1">
            {canAdd && (
              <Button
                variant="primary"
                size="md"
                disabled={!matches || holding || phase === "approving"}
                onClick={() => void approve()}
              >
                {phase === "approving" ? "Waiting for your passkey…" : "Add this phone"}
              </Button>
            )}
            <Button size="md" disabled={phase === "approving"} onClick={() => void decline()}>
              {canAdd ? "Not my phone" : "Close"}
            </Button>
          </div>
        </div>
      )}

      {phase === "aborted" && (
        <div role="alert" className="space-y-2 text-xs" data-testid="add-phone-aborted">
          <p>
            <strong>Two phones answered this code.</strong> Someone else may have seen it. Nothing was added. Start
            again, and keep the code off shared screens.
          </p>
          <div className="flex gap-2">
            <Button variant="primary" size="md" onClick={() => void showCode()}>
              Show a new code
            </Button>
            <Button size="md" onClick={close}>
              Cancel
            </Button>
          </div>
        </div>
      )}

      {phase === "expired" && (
        <div className="space-y-2 text-xs" data-testid="add-phone-expired">
          <p>This code expired.</p>
          <div className="flex gap-2">
            <Button variant="primary" size="md" onClick={() => void showCode()}>
              Show a new code
            </Button>
            <Button size="md" onClick={close}>
              Cancel
            </Button>
          </div>
        </div>
      )}

      {phase !== "checking" && (
        <div className="space-y-2 border-t pt-2" style={{ borderColor: "var(--ligus-line)" }}>
          {!unavailable && (
            <LinkButton onClick={() => setPaste((v) => !v)}>{paste ? "Hide the phone's code" : "Can't scan?"}</LinkButton>
          )}
          {paste && <PastePhoneCode onDone={onDone} onCancel={unavailable ? onCancel : undefined} />}
        </div>
      )}
    </div>
  );
}

/// "ABCD-EF01-…" read out group by group: "A B C D, E F 0 1, …".
export function spellFingerprint(fp: string): string {
  return `Fingerprint ${fp
    .split("-")
    .map((g) => g.split("").join(" "))
    .join(", ")}`;
}

/// **Can't scan?**: type or paste the code the phone app shows (Settings >
/// This phone), compare the fingerprint with the phone's screen, then
/// confirm with your passkey.
export function PastePhoneCode({ onDone, onCancel }: { onDone: () => void; onCancel?: () => void }) {
  const [code, setCode] = useState("");
  const [preview, setPreview] = useState<PhoneCodeDto | null>(null);
  const [matches, setMatches] = useState(false);
  const [busy, setBusy] = useState(false);
  // The code the textarea holds now: a check that answers after the code
  // changed is dropped, so the sheet never shows another code's phone.
  const latest = useRef("");

  async function check(next: string) {
    latest.current = next;
    setCode(next);
    setPreview(null);
    setMatches(false);
    if (!next.trim()) return;
    try {
      const got = await inspectPhoneCode(next);
      if (latest.current === next) setPreview(got);
    } catch (e) {
      if (latest.current === next && next.trim().startsWith("ligusphone1:")) toast.error(errMsg(e));
    }
  }

  async function certify() {
    if (!preview) return;
    setBusy(true);
    try {
      // Certify only the key whose fingerprint was compared, into the
      // workspace the preview was for (refused if the active one changed).
      await certifyPhoneCode(code, preview.fingerprint, preview.workspaceId);
      toast.success(
        `${preview.label || "Your phone"} is now one of your devices. Give your box a few seconds to sync it, then tap Retry on the phone (or scan a new \`ligus pair\` link if the old one used its five attempts).`,
      );
      onDone();
    } catch (e) {
      toast.error(`Couldn't add the phone: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }

  // A phone that is already yours can be added again: nothing is written to
  // the workspace, and My devices takes the name it gives itself now.
  const blocked = !preview || (preview.standing !== "new" && preview.standing !== "alreadyMine");
  return (
    <div className="space-y-2" data-testid="add-phone-paste">
      <p className="text-xs text-ink-soft">
        In the phone app, open <em>Settings &gt; This phone</em> and show its code, then type it here (or paste it, if
        you have it on this computer).
      </p>
      <textarea
        value={code}
        onChange={(e) => void check(e.target.value)}
        placeholder="The phone's code: ligusphone1:…"
        rows={3}
        className="w-full rounded-xl border px-3 py-2 font-mono text-xs"
        style={fieldStyle}
        aria-label="Phone code"
      />
      {preview && (
        <div className="space-y-1.5 text-xs">
          <div>
            <span className="text-ink-soft">Phone</span> <strong>{preview.label || "unnamed"}</strong>
            <span className="text-ink-soft" data-testid="paste-phone-name-hint">
              {" "}
              (the name the phone gives itself; the fingerprint is what proves it's yours)
            </span>
          </div>
          <div>
            <span className="text-ink-soft">Fingerprint</span>{" "}
            <code
              className="font-bold tracking-wider"
              data-testid="paste-phone-fingerprint"
              aria-label={spellFingerprint(preview.fingerprint)}
            >
              {preview.fingerprint}
            </code>
          </div>
          <div className="truncate font-mono text-[11px] text-ink-soft">device {preview.deviceId}</div>
          {preview.standing === "alreadyMine" && (
            <div className="text-ink-soft" data-testid="paste-phone-already-mine">
              Already one of your devices. Adding it again keeps it and updates its name in My devices.
            </div>
          )}
          {preview.problem && <div style={{ color: "var(--ligus-danger-ink)" }}>{preview.problem}</div>}
          {(preview.standing === "new" || preview.standing === "alreadyMine") && (
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={matches} onChange={(e) => setMatches(e.target.checked)} />
              The phone's screen shows the same fingerprint
            </label>
          )}
        </div>
      )}
      <div className="flex gap-2">
        <Button variant="primary" size="md" disabled={blocked || !matches || busy} onClick={() => void certify()}>
          {busy ? "Waiting for your passkey…" : "Add this phone"}
        </Button>
        {onCancel && (
          <Button size="md" onClick={onCancel}>
            Cancel
          </Button>
        )}
      </div>
    </div>
  );
}
