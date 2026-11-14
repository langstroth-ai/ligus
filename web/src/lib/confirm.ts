import { confirmDialog } from "@/components/Dialog";

/// Guard a destructive action behind an in-app confirm dialog. Keeps call sites
/// terse: `onClick={() => confirmThen("Remove X?", () => mut.mutate(id))}`.
/// Uses the in-app dialog (NOT window.confirm, which is a no-op in Tauri
/// webviews — that made these buttons dead in release builds).
export function confirmThen(message: string, run: () => void) {
  void confirmDialog(message, { danger: true }).then((ok) => {
    if (ok) run();
  });
}

/// The start of the desktop's refusal to seal a key to devices only the
/// relay vouches for (invite by handle, a new DM).
export const UNVERIFIED_RECIPIENT = "unverified recipient";

/// Run `call(false)`; when the desktop refuses because the recipient's device
/// keys are vouched for by the relay alone, warn that the relay could read
/// what is sealed to them and, only if the person confirms, run `call(true)`.
/// Resolves to `null` when they cancel.
export async function withUnverifiedRecipientConfirm<T>(
  who: string,
  call: (confirmUnverified: boolean) => Promise<T>,
): Promise<T | null> {
  try {
    return await call(false);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (!msg.startsWith(UNVERIFIED_RECIPIENT)) throw e;
    const ok = await confirmDialog(
      `Nothing on this device confirms that ${who}'s device keys are theirs: only the relay vouches for them. ` +
        `If the relay is compromised, it could read what you seal to these keys (for an invite, the whole workspace). ` +
        `Continue only if you trust this relay, or ask ${who} to join a workspace you share first.`,
      { title: "Unverified recipient", confirmLabel: "Seal anyway", danger: true },
    );
    return ok ? await call(true) : null;
  }
}
