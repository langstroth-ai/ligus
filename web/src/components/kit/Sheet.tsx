// A sheet: a dialog that slides in from the right edge or sits centred, for
// multi-step flows (add a phone, admit a box, start a session). Built on
// Modal, so focus stays inside, Esc closes and focus returns on close.

import { useId } from "react";
import type { ReactNode } from "react";
import { IconX } from "@/lib/icons";
import { Modal } from "@/components/ui";
import { Stepper } from "./Stepper";
import "./kit.css";

export function Sheet({
  title,
  description,
  side = "right",
  width,
  steps,
  step = 0,
  footer,
  onClose,
  titleAddon,
  corner,
  children,
}: {
  title: string;
  /// Beside the title (a "?").
  titleAddon?: ReactNode;
  /// Quiet, in the header's corner before Close (a countdown).
  corner?: ReactNode;
  description?: ReactNode;
  /// "right": a full-height panel on the right edge; "center": a centred card.
  side?: "right" | "center";
  width?: number;
  /// Step names; shows a Stepper under the title.
  steps?: string[];
  /// The current step (0-based) when `steps` is set.
  step?: number;
  /// Actions pinned to the bottom (Back, Next, Done).
  footer?: ReactNode;
  onClose: () => void;
  children: ReactNode;
}) {
  const titleId = useId();
  const right = side === "right";
  const w = width ?? (right ? 440 : 520);
  return (
    <Modal
      onClose={onClose}
      labelledBy={titleId}
      overlayClassName={`ligus-sheet-overlay z-[900] flex ${right ? "justify-end" : "items-center justify-center p-4"}`}
      overlayStyle={{ background: "color-mix(in srgb, #000 28%, transparent)" }}
      panelClassName={`${right ? "ligus-sheet-right h-full" : "ligus-sheet-center max-h-[88vh] rounded-2xl"} flex flex-col border`}
      panelStyle={{
        width: `min(${w}px, 100vw)`,
        background: "var(--ligus-panel)",
        borderColor: "var(--ligus-line)",
        boxShadow: "var(--ligus-shadow-pop)",
        color: "var(--ligus-ink)",
      }}
    >
      <>
        <header className="flex items-start gap-3 border-b px-5 pb-3 pt-4" style={{ borderColor: "var(--ligus-line)" }}>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h2 id={titleId} className="text-[17px] font-semibold leading-6">
                {title}
              </h2>
              {titleAddon}
            </div>
            {description && (
              <p className="mt-0.5 text-[13px]" style={{ color: "var(--ligus-ink-soft)" }}>
                {description}
              </p>
            )}
            {steps && steps.length > 1 && (
              <div className="mt-3">
                <Stepper steps={steps} current={step} />
              </div>
            )}
          </div>
          {corner && <div className="shrink-0 pt-1">{corner}</div>}
          <button
            type="button"
            aria-label="Close"
            onClick={onClose}
            className="ligus-press -mr-1 inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-lg"
            style={{ color: "var(--ligus-ink-soft)" }}
          >
            <IconX size={16} />
          </button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">{children}</div>
        {footer && (
          <footer className="flex items-center justify-end gap-2 border-t px-5 py-3" style={{ borderColor: "var(--ligus-line)" }}>
            {footer}
          </footer>
        )}
      </>
    </Modal>
  );
}
