import { AlertTriangle, Info, XCircle } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "./ui/primitives";

/** The app mark: a drive with an activity light. Monochrome, follows the theme. */
export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={cn("size-6", className)} aria-hidden fill="none">
      <rect x="2.5" y="6" width="19" height="12" rx="4" className="fill-accent" />
      <circle cx="17" cy="12" r="1.6" className="fill-accent-fg" />
      <rect x="6" y="11" width="6.5" height="2" rx="1" className="fill-accent-fg" opacity="0.55" />
    </svg>
  );
}

export function PageHeader({ title, description, actions }: { title: string; description?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="flex flex-wrap items-end justify-between gap-x-6 gap-y-3 pb-6">
      <div className="min-w-0">
        <h1 className="text-[22px] font-semibold tracking-[-0.02em] text-fg">{title}</h1>
        {description && <p className="mt-1 max-w-[560px] text-[13.5px] text-fg-2">{description}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}

type BannerTone = "info" | "warn" | "danger";

const bannerStyles: Record<BannerTone, { box: string; icon: ReactNode }> = {
  info: { box: "border-border bg-surface", icon: <Info className="size-[18px] text-fg-2" /> },
  warn: { box: "border-warn/25 bg-warn-soft", icon: <AlertTriangle className="size-[18px] text-warn" /> },
  danger: { box: "border-danger/25 bg-danger-soft", icon: <XCircle className="size-[18px] text-danger" /> },
};

export function Banner({ tone = "info", title, children, actions, className }: {
  tone?: BannerTone;
  title: string;
  children?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  const s = bannerStyles[tone];
  return (
    <section
      className={cn("anim-fade flex flex-wrap items-start gap-3 rounded-[16px] border px-4 py-3.5", s.box, className)}
      aria-label={title}
    >
      <div className="mt-px shrink-0">{s.icon}</div>
      <div className="min-w-[220px] flex-1">
        <p className="text-[13.5px] font-medium text-fg">{title}</p>
        {children && <div className="mt-0.5 text-[13px] leading-relaxed text-fg-2">{children}</div>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2 self-center">{actions}</div>}
    </section>
  );
}

/** A labelled group of settings rows inside a card. */
export function SettingsGroup({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2" aria-label={title}>
      <div className="px-1">
        <h2 className="text-[13.5px] font-semibold text-fg">{title}</h2>
        {description && <p className="text-[12.5px] text-fg-3">{description}</p>}
      </div>
      <div className="divide-y divide-border rounded-[var(--radius-card)] border border-border bg-surface px-5 shadow-[var(--shadow-card)]">
        {children}
      </div>
    </section>
  );
}

export function SettingsRow({ label, description, children, htmlFor }: {
  label: string;
  description?: ReactNode;
  children: ReactNode;
  htmlFor?: string;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2 py-3.5">
      <div className="min-w-[200px] flex-1">
        <label htmlFor={htmlFor} className="text-[13.5px] font-medium text-fg">
          {label}
        </label>
        {description && <p className="mt-0.5 text-[12.5px] leading-relaxed text-fg-3">{description}</p>}
      </div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

export function Select<T extends string | number>({ id, value, onChange, options, label, className }: {
  id?: string;
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: string; disabled?: boolean }[];
  label?: string;
  className?: string;
}) {
  return (
    <div className={cn("relative", className)}>
      <select
        id={id}
        aria-label={label}
        value={String(value)}
        onChange={(e) => {
          const raw = e.target.value;
          const found = options.find((o) => String(o.value) === raw);
          if (found) onChange(found.value);
        }}
        className={cn(
          "h-9 w-full appearance-none rounded-[var(--radius-control)] border border-border bg-surface pl-3 pr-8 text-[13.5px] text-fg",
          "outline-none transition-[border-color,box-shadow] duration-150 hover:border-border-strong",
          "focus:border-border-strong focus:shadow-[0_0_0_3px_var(--ring-soft)]",
        )}
      >
        {options.map((o) => (
          <option key={String(o.value)} value={String(o.value)} disabled={o.disabled}>
            {o.label}
          </option>
        ))}
      </select>
      <svg
        viewBox="0 0 16 16"
        className="pointer-events-none absolute right-2.5 top-1/2 size-3.5 -translate-y-1/2 text-fg-3"
        aria-hidden
      >
        <path d="M4 6l4 4 4-4" stroke="currentColor" strokeWidth="1.6" fill="none" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </div>
  );
}
