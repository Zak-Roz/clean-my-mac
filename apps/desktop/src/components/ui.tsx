// Small building blocks with a compact, native macOS look.

import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import { type ButtonHTMLAttributes, type ReactNode, useEffect, useRef } from "react";
import type { Risk } from "../bindings/Risk";
import { RISK_META } from "../lib/findings";
import { CheckCircleIcon, InfoIcon, WarningIcon } from "./icons";

export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

type ButtonVariant = "primary" | "secondary" | "ghost" | "destructive";

const BUTTON_STYLES: Record<ButtonVariant, string> = {
  primary: "bg-accent text-on-accent hover:bg-accent-hover shadow-sm disabled:opacity-45",
  secondary:
    "bg-surface text-fg border border-line-strong shadow-[0_0.5px_1px_rgb(0_0_0/0.08)] hover:bg-surface-2 disabled:opacity-45",
  ghost: "text-accent hover:bg-accent-soft disabled:opacity-45",
  destructive: "bg-critical text-white hover:brightness-110 shadow-sm disabled:opacity-45",
};

export function Button({
  variant = "secondary",
  size = "md",
  className,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: ButtonVariant; size?: "sm" | "md" }) {
  return (
    <button
      type="button"
      className={cx(
        "inline-flex items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap transition-colors disabled:pointer-events-none",
        size === "sm" ? "h-6 px-2 text-[12px]" : "h-7 px-3 text-[13px]",
        BUTTON_STYLES[variant],
        className,
      )}
      {...rest}
    />
  );
}

export function IconButton({
  label,
  className,
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <Tip content={label}>
      <button
        type="button"
        aria-label={label}
        className={cx(
          "inline-flex size-6 shrink-0 items-center justify-center rounded-md text-fg-2 hover:bg-surface-3 hover:text-fg",
          className,
        )}
        {...rest}
      >
        {children}
      </button>
    </Tip>
  );
}

const RISK_BADGE: Record<Risk, string> = {
  safe: "bg-good-soft text-good-text",
  review: "bg-warn-soft text-warn-text",
  manual: "bg-critical-soft text-critical-text",
};

export function RiskBadge({ risk }: { risk: Risk }) {
  const meta = RISK_META[risk];
  return (
    <Tip content={meta.hint}>
      <span
        className={cx(
          "inline-flex h-[18px] shrink-0 items-center rounded px-1.5 text-[11px] font-semibold",
          RISK_BADGE[risk],
        )}
      >
        {meta.label}
      </span>
    </Tip>
  );
}

export function Chip({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span
      title={title}
      className="inline-flex h-[18px] shrink-0 items-center rounded border border-line px-1.5 text-[11px] text-fg-2"
    >
      {children}
    </span>
  );
}

export function Card({ className, children }: { className?: string; children: ReactNode }) {
  return (
    <section className={cx("rounded-xl border border-line bg-surface shadow-[0_1px_2px_rgb(0_0_0/0.04)]", className)}>
      {children}
    </section>
  );
}

export function CardHeader({ title, aside }: { title: ReactNode; aside?: ReactNode }) {
  return (
    <header className="flex min-h-9 items-center justify-between gap-3 px-4 pt-3">
      <h2 className="text-[13px] font-semibold">{title}</h2>
      {aside && <div className="flex items-center gap-2 text-[12px] text-fg-2">{aside}</div>}
    </header>
  );
}

type Tone = "info" | "warn" | "critical" | "good";

const TONE: Record<Tone, { box: string; icon: string; Icon: typeof InfoIcon }> = {
  info: { box: "bg-accent-soft", icon: "text-accent", Icon: InfoIcon },
  warn: { box: "bg-warn-soft", icon: "text-warn", Icon: WarningIcon },
  critical: { box: "bg-critical-soft", icon: "text-critical", Icon: WarningIcon },
  good: { box: "bg-good-soft", icon: "text-good", Icon: CheckCircleIcon },
};

/** Inline message box: icon + text, never color alone. */
export function Callout({
  tone = "info",
  title,
  children,
  action,
  className,
}: {
  tone?: Tone;
  title?: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  const t = TONE[tone];
  return (
    <div
      role={tone === "critical" ? "alert" : undefined}
      className={cx("flex items-start gap-2.5 rounded-lg px-3 py-2.5", t.box, className)}
    >
      <t.Icon className={cx("mt-px shrink-0", t.icon)} />
      <div className="min-w-0 flex-1 text-[12.5px] leading-snug">
        {title && <div className="font-semibold">{title}</div>}
        {children && <div className={cx("selectable", title ? "text-fg-2" : "text-fg")}>{children}</div>}
      </div>
      {action && <div className="shrink-0 self-center">{action}</div>}
    </div>
  );
}

/** Status pill: icon + label, color is only a second channel. */
export function StatusPill({ tone, children }: { tone: Exclude<Tone, "info">; children: ReactNode }) {
  const t = TONE[tone];
  return (
    <span
      className={cx(
        "inline-flex h-[22px] items-center gap-1 rounded-full px-2 text-[12px] font-medium",
        t.box,
        tone === "good" ? "text-good-text" : tone === "warn" ? "text-warn-text" : "text-critical-text",
      )}
    >
      <t.Icon size={13} />
      {children}
    </span>
  );
}

export function Segmented<T extends string | number>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { value: T; label: ReactNode }[];
  onChange: (value: T) => void;
  label: string;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="inline-flex h-7 items-center rounded-md bg-surface-3 p-0.5">
      {options.map((o) => {
        const active = o.value === value;
        return (
          // biome-ignore lint/a11y/useSemanticElements: a segmented control is a radiogroup of buttons
          <button
            key={String(o.value)}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChange(o.value)}
            className={cx(
              "inline-flex h-6 items-center gap-1 rounded-[5px] px-2.5 text-[12px] font-medium whitespace-nowrap",
              active ? "bg-surface text-fg shadow-[0_1px_2px_rgb(0_0_0/0.12)]" : "text-fg-2 hover:text-fg",
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/** Native checkbox with tri-state support. */
export function Checkbox({
  checked,
  indeterminate = false,
  disabled,
  onChange,
  label,
}: {
  checked: boolean;
  indeterminate?: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
  label: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate;
  }, [indeterminate]);
  return (
    <input
      ref={ref}
      type="checkbox"
      aria-label={label}
      checked={checked}
      disabled={disabled}
      onChange={(e) => onChange(e.target.checked)}
      className="shrink-0 disabled:opacity-40"
    />
  );
}

export function TooltipProvider({ children }: { children: ReactNode }) {
  return (
    <TooltipPrimitive.Provider delayDuration={450} skipDelayDuration={200}>
      {children}
    </TooltipPrimitive.Provider>
  );
}

export function Tip({
  content,
  children,
  side = "top",
}: {
  content: ReactNode;
  children: ReactNode;
  side?: "top" | "bottom" | "left" | "right";
}) {
  if (!content) return <>{children}</>;
  return (
    <TooltipPrimitive.Root>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          side={side}
          sideOffset={5}
          collisionPadding={8}
          className="z-50 max-w-72 animate-fade-in rounded-md border border-line bg-surface px-2 py-1 text-[12px] leading-snug text-fg shadow-lg"
        >
          {content}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}

export function Spinner({ size = 14 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" className="animate-spin text-fg-2" aria-hidden="true">
      <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2" />
      <path d="M14 8a6 6 0 0 0-6-6" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    </svg>
  );
}

export function ProgressBar({ value, label }: { value: number | null; label: string }) {
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value === null ? undefined : Math.round(value * 100)}
      className="relative h-1.5 w-full overflow-hidden rounded-full bg-track"
    >
      {value === null ? (
        <div className="absolute inset-y-0 w-2/5 animate-indeterminate rounded-full bg-accent" />
      ) : (
        <div
          className="h-full rounded-full bg-accent transition-[width] duration-200"
          style={{ width: `${Math.max(2, value * 100)}%` }}
        />
      )}
    </div>
  );
}

export function EmptyState({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 px-6 py-10 text-center">
      {icon && <div className="text-fg-3">{icon}</div>}
      <div className="text-[13px] font-semibold">{title}</div>
      {children && <div className="max-w-md text-[12.5px] text-fg-2">{children}</div>}
    </div>
  );
}
