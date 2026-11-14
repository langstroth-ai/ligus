// Shared UI primitives — one button/section language for the whole app.
// The audit found three competing button implementations and four section
// header treatments; every surface should compose these instead.
//
// Radius scale (app-wide): controls = rounded-xl, cards = rounded-2xl,
// hero surfaces = rounded-3xl.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ButtonHTMLAttributes, CSSProperties, ReactNode, RefObject } from "react";
import { IconAlertTriangle } from "@/lib/icons";

type Variant = "primary" | "ghost" | "danger";
type Size = "sm" | "md";

const SIZE: Record<Size, string> = {
  sm: "h-8 px-3 text-sm",
  md: "h-9 px-4 text-sm",
};

/// The app button. `primary` = accent call-to-action, `ghost` = neutral
/// control on mist, `danger` = destructive.
export function Button({
  variant = "ghost",
  size = "sm",
  className = "",
  style,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: Size }) {
  const base = `inline-flex shrink-0 items-center justify-center gap-1.5 rounded-xl font-medium transition-all disabled:cursor-not-allowed disabled:opacity-40 ${SIZE[size]}`;
  const variants: Record<Variant, { className: string; style: React.CSSProperties }> = {
    primary: {
      className: "shadow-sm hover:brightness-105 disabled:shadow-none",
      style: { background: "var(--hive-accent-fill)", color: "var(--hive-on-accent)" },
    },
    ghost: {
      className: "border hover:border-[color:var(--hive-accent-cool)]",
      style: {
        borderColor: "var(--hive-line)",
        background: "var(--hive-mist)",
        color: "var(--hive-ink)",
      },
    },
    danger: {
      className: "border hover:brightness-110",
      style: {
        borderColor: "var(--hive-danger-line)",
        background: "var(--hive-danger-tint)",
        color: "var(--hive-danger-ink)",
      },
    },
  };
  const v = variants[variant];
  return (
    <button
      {...rest}
      className={`${base} ${v.className} ${className}`}
      style={{ ...v.style, ...style }}
    />
  );
}

/// Square icon-only button. The aria-label is required — icon buttons were
/// the app's biggest accessibility gap.
export function IconButton({
  label,
  size = 32,
  className = "",
  children,
  ...rest
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> & {
  label: string;
  size?: number;
  children: ReactNode;
}) {
  return (
    <button
      {...rest}
      aria-label={label}
      title={rest.title ?? label}
      className={`inline-flex shrink-0 items-center justify-center rounded-lg opacity-70 transition-all hover:opacity-100 hover:bg-[color:var(--hive-overlay)] disabled:cursor-not-allowed disabled:opacity-30 ${className}`}
      style={{ width: size, height: size, color: "var(--hive-ink)", ...rest.style }}
    >
      {children}
    </button>
  );
}

/// One section-header treatment for panes/settings: small-caps title, an
/// optional trailing action, consistent spacing.
export function Section({
  title,
  action,
  children,
  className = "",
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`mb-5 ${className}`}>
      <div className="mb-2 flex items-center justify-between gap-2">
        <h2 className="text-[13px] font-semibold tracking-[-0.01em] opacity-90">{title}</h2>
        {action}
      </div>
      <div className="space-y-2.5">{children}</div>
    </section>
  );
}

// Selector for everything a modal's Tab cycle may land on.
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function focusables(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => el.getClientRects().length > 0 || el === document.activeElement,
  );
}

// Stack of open modals so only the top-most one reacts to Escape/Tab when
// modals nest (e.g. a confirm dialog launched from inside another modal).
const modalStack: symbol[] = [];

/// Modal shell: overlay + panel with the focus management every dialog needs.
/// - Tab / Shift-Tab cycle inside the panel (focus can't escape behind the overlay)
/// - Escape closes, document-wide — no element inside needs focus
/// - On open, focuses the child that used `autoFocus`, else the first control
/// - On close, focus returns to whatever had it before the modal opened
/// - Clicking the overlay closes; clicks inside the panel don't propagate out
/// Render it conditionally (`open && <Modal …>`); it assumes it is mounted open.
/// Consumers style layering/placement via overlayClassName and the panel via
/// panelClassName/panelStyle, so existing looks carry over unchanged.
export function Modal({
  onClose,
  overlayClassName = "z-[900] flex items-center justify-center p-4",
  overlayStyle,
  panelClassName = "",
  panelStyle,
  children,
}: {
  onClose: () => void;
  overlayClassName?: string;
  overlayStyle?: CSSProperties;
  panelClassName?: string;
  panelStyle?: CSSProperties;
  children: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const id = Symbol("modal");
    modalStack.push(id);
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const panel = panelRef.current;
    if (!panel) return;

    // React runs children's autoFocus during mount, before this effect — if a
    // child already claimed focus, respect it; otherwise take the first control.
    if (!panel.contains(document.activeElement)) {
      (focusables(panel)[0] ?? panel).focus();
    }

    const onKeyDown = (e: KeyboardEvent) => {
      if (modalStack[modalStack.length - 1] !== id) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCloseRef.current();
      } else if (e.key === "Tab") {
        const nodes = focusables(panel);
        if (nodes.length === 0) {
          e.preventDefault();
          panel.focus();
          return;
        }
        const first = nodes[0];
        const last = nodes[nodes.length - 1];
        const active = document.activeElement;
        if (e.shiftKey) {
          if (active === first || !panel.contains(active)) {
            e.preventDefault();
            last.focus();
          }
        } else if (active === last || !panel.contains(active)) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    // Capture phase so the trap holds even if a child handler stops propagation.
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      modalStack.splice(modalStack.indexOf(id), 1);
      prev?.focus();
    };
  }, []);

  return (
    <div
      className={`fixed inset-0 ${overlayClassName}`}
      style={overlayStyle}
      onClick={() => onCloseRef.current()}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        className={panelClassName}
        style={{ outline: "none", ...panelStyle }}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

/// Card surface for grouped content inside a pane (radius scale: 2xl).
export function Card({
  children,
  className = "",
  style,
}: {
  children: ReactNode;
  className?: string;
  style?: React.CSSProperties;
}) {
  return (
    <div
      className={`rounded-xl border ${className}`}
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", ...style }}
    >
      {children}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────
// Redesign primitives (design spec §5). All color is a token or a color-mix
// of one (R1); the dense type ramp (§3) lives in these so sidebar/rail lists
// read at 13/590 instead of body scale.
// ─────────────────────────────────────────────────────────────────────────

/// Shared style tokens promoted out of RightRail's private copies (R2).
export const panelStyle: CSSProperties = {
  borderColor: "var(--hive-line)",
  background: "var(--hive-panel)",
};
export const fieldStyle: CSSProperties = {
  borderColor: "var(--hive-line)",
  // backgroundColor (not the `background` shorthand) so the global `select`
  // rule's chevron background-image isn't wiped on dropdowns using this style.
  backgroundColor: "var(--hive-mist)",
  color: "var(--hive-ink)",
};
/// Cool selection tint + border (the exact §2 recipe), reused by active rows.
export const SELECT_TINT = "color-mix(in srgb, var(--hive-accent-cool) 14%, transparent)";
export const SELECT_LINE = "color-mix(in srgb, var(--hive-accent-cool) 38%, transparent)";

/// §5.1 — the one 30px list row for every sidebar and pane entry. `active`
/// paints the cool selection tint; `attn` turns the count into a warm pill.
export function NavRow({
  icon,
  label,
  count,
  meter,
  attn = false,
  active = false,
  onClick,
  children,
  title,
}: {
  icon?: ReactNode;
  label: string;
  count?: number;
  meter?: number;
  attn?: boolean;
  active?: boolean;
  onClick?: () => void;
  children?: ReactNode;
  title?: string;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className="flex h-[30px] w-full items-center gap-2.5 rounded-xl px-2.5 text-left transition-colors hover:bg-[color:var(--hive-overlay)]"
      style={active ? { background: SELECT_TINT } : undefined}
    >
      {icon != null && (
        <span
          className="grid h-4 w-4 shrink-0 place-items-center"
          style={{ color: active ? "var(--hive-accent-ink)" : "var(--hive-ink-soft)" }}
          aria-hidden
        >
          {icon}
        </span>
      )}
      {children}
      <span
        className="min-w-0 flex-1 truncate text-[13px]"
        style={{ fontWeight: active ? 600 : 400, color: "var(--hive-ink)" }}
      >
        {label}
      </span>
      {typeof meter === "number" && (
        <span
          className="h-1 w-8 shrink-0 overflow-hidden rounded-full"
          style={{ background: "color-mix(in srgb, var(--hive-ink) 14%, transparent)" }}
        >
          <span
            className="block h-full"
            style={{ width: `${Math.max(0, Math.min(1, meter)) * 100}%`, background: "var(--hive-accent-cool)" }}
          />
        </span>
      )}
      {typeof count === "number" &&
        (attn ? (
          <span
            className="shrink-0 rounded-full px-1.5 text-[10.5px] font-bold tabular-nums"
            style={{ background: "var(--hive-warm-fill)", color: "var(--hive-on-accent)" }}
          >
            {count}
          </span>
        ) : (
          <span className="shrink-0 text-[11.5px] tabular-nums" style={{ color: "var(--hive-ink-soft)" }}>
            {count}
          </span>
        ))}
    </button>
  );
}

/// §3 dense section cap — 11px / 700 / 0.08em uppercase.
export function SectionCap({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex h-[26px] items-center justify-between px-2.5">
      <span
        className="text-[11px] font-bold uppercase"
        style={{ letterSpacing: "0.08em", color: "var(--hive-ink-soft)" }}
      >
        {children}
      </span>
      {action}
    </div>
  );
}

/// §5.2 — the two-line, 46px chat row. Shows recency + participants, never a
/// message count. `options` renders a hover ⋯ button; `menu` is its Popover.
export function ChatRow({
  chatId,
  title,
  when,
  unread = 0,
  active = false,
  onClick,
  onOptions,
  onContextMenu,
  menu,
  avatars,
  sub,
}: {
  /// Rendered as data-chat-id so keyboard next/previous chat follows the
  /// sidebar's own order.
  chatId?: string;
  title: string;
  when?: string;
  unread?: number;
  active?: boolean;
  onClick?: () => void;
  onOptions?: (anchor: HTMLElement) => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  menu?: ReactNode;
  avatars?: ReactNode;
  sub?: ReactNode;
}) {
  return (
    <div className="group/crow relative" onContextMenu={onContextMenu}>
      <button
        onClick={onClick}
        data-chat-id={chatId}
        aria-current={active ? "page" : undefined}
        className="flex min-h-[46px] w-full flex-col justify-center gap-[3px] rounded-xl px-2.5 py-1.5 text-left transition-colors hover:bg-[color:var(--hive-overlay)]"
        style={active ? { background: SELECT_TINT } : undefined}
      >
        <span className="flex items-center gap-2">
          {unread > 0 && (
            <span
              className="h-1.5 w-1.5 shrink-0 rounded-full"
              style={{ background: "var(--hive-accent-cool)" }}
              aria-hidden
            />
          )}
          <span
            className="min-w-0 flex-1 truncate text-[13px]"
            style={{ fontWeight: unread > 0 ? 700 : 590, color: "var(--hive-ink)", letterSpacing: "-0.01em" }}
          >
            {title}
          </span>
          {when && (
            <span className="shrink-0 text-[11px] tabular-nums" style={{ color: "var(--hive-ink-soft)" }}>
              {when}
            </span>
          )}
        </span>
        {(avatars || sub) && (
          <span className="flex items-center gap-2 text-[11px]" style={{ color: "var(--hive-ink-soft)" }}>
            {avatars}
            {sub && <span className="min-w-0 truncate">{sub}</span>}
          </span>
        )}
      </button>
      {onOptions && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onOptions(e.currentTarget);
          }}
          aria-label="Chat options"
          title="Chat options"
          className="absolute right-1.5 top-1.5 grid h-[22px] w-[22px] place-items-center rounded-md opacity-0 transition-opacity hover:bg-[color:var(--hive-overlay)] group-hover/crow:opacity-100"
          style={{ color: "var(--hive-ink-soft)" }}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
            <circle cx="5" cy="12" r="1.4" />
            <circle cx="12" cy="12" r="1.4" />
            <circle cx="19" cy="12" r="1.4" />
          </svg>
        </button>
      )}
      {menu}
    </div>
  );
}

/// §5.6 — the app switch. Replaces raw <input type=checkbox>.
export function Switch({
  on,
  onChange,
  label,
  disabled = false,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!on)}
      className="relative inline-flex h-5 w-[34px] shrink-0 items-center rounded-full transition-colors disabled:opacity-40"
      style={{
        background: on ? "var(--hive-accent-fill)" : "color-mix(in srgb, var(--hive-ink) 22%, transparent)",
      }}
    >
      <span
        className="absolute h-4 w-4 rounded-full transition-all"
        style={{
          left: on ? 16 : 2,
          background: on ? "var(--hive-on-accent)" : "var(--hive-panel)",
          boxShadow: "var(--hive-shadow-1)",
        }}
      />
    </button>
  );
}

/// §5.5 — three-segment permission picker (never a dropdown).
const PERMS: { id: "auto" | "ask" | "off"; label: string; on: CSSProperties }[] = [
  { id: "auto", label: "Auto", on: { background: "color-mix(in srgb, var(--hive-success) 20%, transparent)", color: "var(--hive-success-ink)" } },
  { id: "ask", label: "Ask", on: { background: "color-mix(in srgb, var(--hive-accent-warm) 18%, transparent)", color: "var(--hive-warm-ink)" } },
  { id: "off", label: "Off", on: { background: "color-mix(in srgb, var(--hive-ink) 12%, transparent)", color: "var(--hive-ink)" } },
];
export function PermissionPicker({
  value,
  onChange,
  compact = false,
  disabled = false,
}: {
  value: "auto" | "ask" | "off";
  onChange?: (v: "auto" | "ask" | "off") => void;
  compact?: boolean;
  disabled?: boolean;
}) {
  return (
    <span
      role="group"
      className="inline-flex rounded-lg border p-0.5"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", opacity: disabled ? 0.55 : 1 }}
    >
      {PERMS.map((p) => {
        const active = value === p.id;
        return (
          <button
            key={p.id}
            type="button"
            disabled={disabled || !onChange}
            title={p.id === "auto" ? "Runs without asking" : p.id === "ask" ? "Prompts each call" : "Never offered to the model"}
            onClick={() => onChange?.(p.id)}
            className={`rounded-md font-medium transition-colors ${compact ? "px-2 py-0.5 text-[11px]" : "px-2.5 py-1 text-xs"}`}
            style={active ? p.on : { color: "var(--hive-ink-soft)", background: "transparent" }}
          >
            {p.label}
          </button>
        );
      })}
    </span>
  );
}

/// §5 Field — label + optional hint over any control.
export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1.5">
      <span className="text-[11.5px] font-semibold" style={{ color: "var(--hive-ink-soft)" }}>
        {label}
      </span>
      {children}
      {hint && (
        <span className="text-[11px]" style={{ color: "var(--hive-ink-soft)" }}>
          {hint}
        </span>
      )}
    </label>
  );
}

/// A native <select> styled to the field token (promoted from RightRail's
/// SubtleSelectField).
export function SelectField({
  value,
  onChange,
  children,
  disabled = false,
  ariaLabel,
}: {
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  return (
    // Native chrome-stripping + the chevron come from the global `select` rule in
    // styles.css, so every dropdown in the app (this and the raw ones) matches.
    <select
      value={value}
      aria-label={ariaLabel}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value)}
      className="w-full cursor-pointer rounded-xl border px-3 py-2 text-sm outline-none transition-colors focus:border-[color:var(--hive-accent-cool)] disabled:cursor-default disabled:opacity-50"
      style={fieldStyle}
    >
      {children}
    </select>
  );
}

/// §7.1 EmptyHint — one terse sentence for an empty list (state matrix §8),
/// with an optional glyph and a single next action.
export function EmptyHint({ text, action, icon }: { text: string; action?: ReactNode; icon?: ReactNode }) {
  return (
    <div
      className="flex flex-col items-center gap-1.5 rounded-xl border border-dashed px-3 py-4 text-center text-xs text-ink-soft"
      style={{ borderColor: "var(--hive-line)", background: "color-mix(in srgb, var(--hive-mist) 55%, transparent)" }}
    >
      {icon && (
        <span aria-hidden className="text-ink-faint">
          {icon}
        </span>
      )}
      <div>{text}</div>
      {action && <div className="mt-1">{action}</div>}
    </div>
  );
}

/// A failed-read card: a danger-toned message plus an optional Retry that
/// re-runs the query's `refetch`. Distinct from `EmptyHint` so a failed load
/// never masquerades as an empty/starter state (which reads as data loss).
export function ErrorState({ text, onRetry }: { text: string; onRetry?: () => void }) {
  return (
    <div
      role="alert"
      className="flex items-start gap-2.5 rounded-xl border px-3 py-2.5 text-xs"
      style={{ borderColor: "var(--hive-danger-line)", background: "var(--hive-danger-tint)", color: "var(--hive-ink)" }}
    >
      <span aria-hidden className="mt-px shrink-0" style={{ color: "var(--hive-danger-ink)" }}>
        <IconAlertTriangle size={14} />
      </span>
      <div className="min-w-0 flex-1 leading-5">{text}</div>
      {onRetry && (
        <button
          onClick={onRetry}
          className="shrink-0 rounded-lg border px-2.5 py-1 text-xs font-medium transition-colors hover:border-[color:var(--hive-accent-cool)]"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
        >
          Retry
        </button>
      )}
    </div>
  );
}

/// Skeleton rows shaped like a pane's cards, so loading never reads as empty
/// and the layout doesn't jump when data lands.
export function LoadingState({ rows = 3, label = "Loading" }: { rows?: number; label?: string }) {
  return (
    <div role="status" aria-live="polite" aria-label={label} className="space-y-2">
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="rounded-xl border px-3 py-3" style={{ borderColor: "var(--hive-line)" }}>
          <div className="hive-skel h-3" style={{ width: `${62 - i * 9}%` }} />
          <div className="hive-skel mt-2 h-2.5" style={{ width: `${38 + i * 7}%` }} />
        </div>
      ))}
    </div>
  );
}

export type Tone = "neutral" | "info" | "success" | "warn" | "danger";

const TONE_STYLE: Record<Tone, CSSProperties> = {
  neutral: { color: "var(--hive-ink-soft)", background: "var(--hive-overlay)", borderColor: "var(--hive-line)" },
  info: { color: "var(--hive-info-ink)", background: "var(--hive-info-tint)", borderColor: "var(--hive-info-line)" },
  success: { color: "var(--hive-success-ink)", background: "var(--hive-success-tint)", borderColor: "var(--hive-success-line)" },
  warn: { color: "var(--hive-warn-ink)", background: "var(--hive-warn-tint)", borderColor: "var(--hive-warn-line)" },
  danger: { color: "var(--hive-danger-ink)", background: "var(--hive-danger-tint)", borderColor: "var(--hive-danger-line)" },
};

/// The one status chip: text always present (never colour alone), an optional
/// leading glyph, ink-on-tint colours that pass AA in every palette.
export function Badge({
  tone = "neutral",
  icon,
  children,
  title,
  className = "",
}: {
  tone?: Tone;
  icon?: ReactNode;
  children: ReactNode;
  title?: string;
  className?: string;
}) {
  return (
    <span
      title={title}
      className={`inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded-full border px-1.5 py-px text-[10.5px] font-semibold leading-4 tracking-[0.02em] ${className}`}
      style={TONE_STYLE[tone]}
    >
      {icon && (
        <span aria-hidden className="inline-flex">
          {icon}
        </span>
      )}
      {children}
    </span>
  );
}

/// A full-width notice strip (workspace banners, chat bars). One height, one
/// padding, one link style wherever it appears; the detail truncates instead
/// of wrapping the title. `tone` sets the icon colour and a faint wash.
export function Banner({
  tone,
  icon,
  title,
  detail,
  actions,
  role = "status",
  testId,
}: {
  tone: Tone;
  icon: ReactNode;
  title: ReactNode;
  detail?: ReactNode;
  actions?: ReactNode;
  role?: "status" | "alert";
  testId?: string;
}) {
  const t = TONE_STYLE[tone];
  return (
    <div
      role={role}
      data-testid={testId}
      className="hive-enter flex min-h-[34px] shrink-0 items-center gap-2.5 border-b px-4 py-1.5 text-[12.5px]"
      style={{
        borderColor: tone === "neutral" ? "var(--hive-line)" : t.borderColor,
        background: tone === "neutral" ? "var(--hive-panel)" : `color-mix(in srgb, ${t.background} 100%, var(--hive-panel))`,
        color: "var(--hive-ink)",
      }}
    >
      <span aria-hidden className="inline-flex shrink-0" style={{ color: t.color }}>
        {icon}
      </span>
      <span className="shrink-0 whitespace-nowrap font-semibold">{title}</span>
      {detail && <span className="min-w-0 flex-1 truncate text-ink-soft">{detail}</span>}
      {!detail && <span className="flex-1" />}
      {actions && <span className="flex shrink-0 items-center gap-3">{actions}</span>}
    </div>
  );
}

/// A text action inside a Banner / card footer: quiet until hovered, visible
/// focus, never colour-only (underline on hover).
export function LinkButton({
  children,
  onClick,
  disabled,
  title,
  tone = "accent",
}: {
  children: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  title?: string;
  tone?: "accent" | "ink";
}) {
  return (
    <button
      type="button"
      title={title}
      disabled={disabled}
      onClick={onClick}
      className="inline-flex items-center gap-1 whitespace-nowrap rounded font-medium underline-offset-2 hover:underline disabled:opacity-50"
      style={{ color: tone === "accent" ? "var(--hive-accent-ink)" : "var(--hive-ink)" }}
    >
      {children}
    </button>
  );
}

/// Presence/health dot that never relies on colour alone: online = filled,
/// offline = hollow ring; the label is announced and shown on hover.
export function StatusDot({ on, label, size = 8 }: { on: boolean; label: string; size?: number }) {
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className="inline-block shrink-0 rounded-full"
      style={{
        width: size,
        height: size,
        background: on ? "var(--hive-success)" : "transparent",
        boxShadow: on ? "0 0 0 2px var(--hive-panel)" : "inset 0 0 0 1.5px var(--hive-ink-faint)",
      }}
    />
  );
}

/// Segmented tabs for a pane's sub-views. Scrolls horizontally instead of
/// wrapping (seven tabs at 300px used to stack three rows); arrow keys move
/// between tabs (roving tabindex).
export function SegmentedTabs<T extends string>({
  tabs,
  value,
  onChange,
  label,
}: {
  tabs: { id: T; label: string; count?: number }[];
  value: T;
  onChange: (id: T) => void;
  label: string;
}) {
  const onKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft" && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const i = tabs.findIndex((t) => t.id === value);
    const next =
      e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1 : (i + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    onChange(tabs[next].id);
    const btn = e.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next];
    btn?.focus();
    btn?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };
  return (
    <div
      role="tablist"
      aria-label={label}
      onKeyDown={onKey}
      className="hive-scroll-x -mx-1 flex gap-0.5 overflow-x-auto px-1 pb-0.5"
    >
      {tabs.map((t) => {
        const on = t.id === value;
        return (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={on}
            tabIndex={on ? 0 : -1}
            onClick={() => onChange(t.id)}
            className="relative shrink-0 whitespace-nowrap rounded-lg px-2.5 py-1 text-[12.5px] font-medium transition-colors hover:bg-[color:var(--hive-hover)]"
            style={{
              color: on ? "var(--hive-ink)" : "var(--hive-ink-soft)",
              background: on ? "var(--hive-mist)" : "transparent",
              boxShadow: on ? "inset 0 0 0 1px var(--hive-line)" : undefined,
            }}
          >
            {t.label}
            {typeof t.count === "number" && t.count > 0 && (
              <span className="ml-1 tabular-nums text-ink-soft">{t.count}</span>
            )}
          </button>
        );
      })}
    </div>
  );
}

/// R6 — "lists lead, forms follow". A `+ Add …` row that discloses its form
/// inline instead of a permanently-expanded form above the list.
export function FormDisclosure({
  label,
  open,
  onToggle,
  children,
}: {
  label: string;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div>
      <button
        onClick={onToggle}
        aria-expanded={open}
        className="flex h-[30px] w-full items-center gap-2 rounded-xl px-2.5 text-left text-[13px] transition-colors hover:bg-[color:var(--hive-overlay)]"
        style={{ color: "var(--hive-accent-ink)" }}
      >
        <span aria-hidden className="text-base leading-none">
          {open ? "×" : "+"}
        </span>
        {label}
      </button>
      {open && <div className="mt-1.5">{children}</div>}
    </div>
  );
}

// ── §5.4 Popover / CandidateList ────────────────────────────────────────────
// One anchored menu + one keyboard reducer for @-mention, slash, routing pill,
// chat options and the workspace context menu. Opens upward when its anchor
// sits in the lower third of the viewport; always renders a click-away backdrop.

/// Keyboard reducer shared by every candidate list (↑↓ move, ↩/⇥ accept, esc
/// dismiss). Wire it to a list's onKeyDown; returns true if it handled the key.
export function candidateKey(
  e: { key: string; preventDefault: () => void },
  opts: {
    count: number;
    active: number;
    setActive: (i: number) => void;
    onAccept: (i: number) => void;
    onDismiss: () => void;
  },
): boolean {
  const { count, active, setActive, onAccept, onDismiss } = opts;
  if (e.key === "ArrowDown") {
    e.preventDefault();
    setActive(Math.min(active + 1, count - 1));
    return true;
  }
  if (e.key === "ArrowUp") {
    e.preventDefault();
    setActive(Math.max(active - 1, 0));
    return true;
  }
  if (e.key === "Enter" || e.key === "Tab") {
    e.preventDefault();
    onAccept(active);
    return true;
  }
  if (e.key === "Escape") {
    e.preventDefault();
    onDismiss();
    return true;
  }
  return false;
}

/// Anchored popover panel. Renders a fixed click-away backdrop, positions
/// relative to its `anchorRef`, and flips upward near the viewport bottom.
export function Popover({
  anchorRef,
  onDismiss,
  minWidth = 240,
  align = "left",
  backdrop = true,
  children,
}: {
  anchorRef: RefObject<HTMLElement | null>;
  onDismiss: () => void;
  minWidth?: number;
  align?: "left" | "right";
  /// A click-away backdrop (default). Discrete menus want it; an autocomplete
  /// anchored to a live input sets it false so the input stays clickable and
  /// relies on Escape / input-state to dismiss.
  backdrop?: boolean;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top?: number; bottom?: number } | null>(null);

  useLayoutEffect(() => {
    const anchor = anchorRef.current;
    if (!anchor) return;
    const r = anchor.getBoundingClientRect();
    const below = window.innerHeight - r.bottom;
    // Open upward when the anchor is in the lower third of the viewport.
    const up = r.bottom > window.innerHeight * 0.67 && below < 260;
    const left = align === "right" ? Math.max(8, r.right - minWidth) : r.left;
    setPos(up ? { left, bottom: window.innerHeight - r.top + 6 } : { left, top: r.bottom + 6 });
  }, [anchorRef, minWidth, align]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismiss();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onDismiss]);

  return (
    <>
      {backdrop && <div className="fixed inset-0 z-[60]" onMouseDown={onDismiss} aria-hidden />}
      <div
        ref={ref}
        role="menu"
        className="fixed z-[61] overflow-hidden rounded-xl border p-1.5 shadow-xl"
        style={{
          minWidth,
          left: pos?.left ?? -9999,
          top: pos?.top,
          bottom: pos?.bottom,
          borderColor: "var(--hive-line)",
          background: "var(--hive-panel)",
          visibility: pos ? "visible" : "hidden",
        }}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </>
  );
}

/// A header label inside a Popover (uppercase caps).
export function PopoverHeader({ children }: { children: ReactNode }) {
  return (
    <div
      className="px-2 pb-1 pt-1 text-[11px] font-semibold uppercase"
      style={{ letterSpacing: "0.06em", color: "var(--hive-ink-soft)" }}
    >
      {children}
    </div>
  );
}

/// One selectable row inside a Popover (glyph/avatar + label + trailing kind).
export function PopoverItem({
  glyph,
  label,
  kind,
  active = false,
  danger = false,
  onSelect,
  onHover,
}: {
  glyph?: ReactNode;
  label: ReactNode;
  kind?: ReactNode;
  active?: boolean;
  danger?: boolean;
  onSelect?: () => void;
  onHover?: () => void;
}) {
  return (
    <button
      role="menuitem"
      onMouseEnter={onHover}
      onMouseDown={(e) => {
        e.preventDefault();
        onSelect?.();
      }}
      className="flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-[13px] transition-colors"
      style={{
        background: active ? SELECT_TINT : "transparent",
        color: danger ? "var(--hive-danger-ink)" : "var(--hive-ink)",
      }}
    >
      {glyph != null && <span className="grid h-5 w-5 shrink-0 place-items-center">{glyph}</span>}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {kind != null && (
        <span className="shrink-0 text-[11px]" style={{ color: "var(--hive-ink-soft)" }}>
          {kind}
        </span>
      )}
    </button>
  );
}
