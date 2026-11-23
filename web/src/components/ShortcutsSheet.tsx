import { Modal } from "@/components/ui";
import { SHORTCUTS, keyLabel, type ShortcutGroup } from "@/lib/shortcuts";

const GROUPS: ShortcutGroup[] = ["General", "Chat", "Panes", "View", "Code"];

/// ⌘/: every keyboard shortcut, read from the same list the key handler
/// uses, so it always matches what the keys do.
export function ShortcutsSheet({ onClose, mac }: { onClose: () => void; mac?: boolean }) {
  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-start justify-center p-4 pt-[10vh]"
      overlayStyle={{ background: "color-mix(in srgb, var(--ligus-ink) 45%, transparent)" }}
      panelClassName="w-full max-w-2xl overflow-hidden rounded-2xl border shadow-2xl"
      panelStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
    >
      <div className="flex items-center justify-between border-b px-5 py-3" style={{ borderColor: "var(--ligus-line)" }}>
        <h2 className="ligus-title">Keyboard shortcuts</h2>
        <button
          type="button"
          onClick={onClose}
          className="rounded-md px-2 py-0.5 text-xs text-ink-soft hover:bg-[color:var(--ligus-overlay)]"
          aria-label="Close keyboard shortcuts"
        >
          Esc
        </button>
      </div>
      <div className="grid max-h-[70vh] gap-x-8 gap-y-5 overflow-y-auto px-5 py-4 sm:grid-cols-2">
        {GROUPS.map((g) => {
          const rows = SHORTCUTS.filter((s) => s.group === g);
          if (rows.length === 0) return null;
          return (
            <section key={g} aria-label={g}>
              <h3 className="ligus-cap mb-1.5">{g}</h3>
              <dl className="flex flex-col gap-1">
                {rows.map((s) => (
                  <div key={`${s.id}-${s.keys}`} className="flex items-baseline justify-between gap-3 text-[13px]">
                    <dt className="min-w-0">{s.label}</dt>
                    <dd className="shrink-0">
                      <kbd
                        className="rounded-md border px-1.5 py-0.5 font-sans text-[11px] tabular-nums"
                        style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-mist)" }}
                      >
                        {keyLabel(s.keys, mac)}
                      </kbd>
                    </dd>
                  </div>
                ))}
              </dl>
            </section>
          );
        })}
      </div>
    </Modal>
  );
}
