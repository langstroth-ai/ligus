import { InfoTip } from "@/components/ui";

/// "I already use Ligus on another device": link this computer to the
/// person's existing account. It never asks for a name and never makes or
/// joins a workspace: this device ends linked, or nothing happens.
///
/// The desktop app can't link itself in this version, so this screen says so
/// plainly and points at what works today on the same computer.
export function LinkThisDevice({ onBack }: { onBack: () => void }) {
  return (
    <div className="space-y-3" data-testid="link-this-device">
      <div className="flex items-center gap-1.5">
        <div className="text-sm font-medium">Link this computer</div>
        <InfoTip
          label="linking this computer"
          text="Starting fresh here would make a second you in your workspaces, with its own name and keys. lt and the ligus CLI on this computer can be linked now: on your other device, copy the address under Settings → Profile → My devices → Add a computer, then run ligus devices request --to followed by that address here (lt: Link to my devices)."
        />
      </div>
      <p className="text-xs text-ink-soft">
        This app can't link itself to your account in this version. Until it can, keep using Ligus on your other
        device.
      </p>
      <button
        type="button"
        onClick={onBack}
        className="w-full rounded-xl px-5 py-2.5 text-sm font-semibold"
        style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
      >
        Back
      </button>
    </div>
  );
}
