import { clsx } from "clsx";
import * as RadixSwitch from "@radix-ui/react-switch";
import * as RadixTooltip from "@radix-ui/react-tooltip";
import { Loader2 } from "lucide-react";
import { forwardRef, useId, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode } from "react";

export const cn = clsx;

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

const base =
  "inline-flex items-center justify-center gap-2 whitespace-nowrap font-medium select-none " +
  "transition-[background-color,border-color,color,box-shadow,transform] duration-150 " +
  "active:translate-y-px disabled:pointer-events-none disabled:opacity-45";

const variants: Record<Variant, string> = {
  primary: "bg-accent text-accent-fg hover:bg-accent-hover shadow-[0_1px_0_rgb(0_0_0/0.05)]",
  secondary: "bg-surface text-fg border border-border hover:border-border-strong hover:bg-surface-2",
  ghost: "text-fg-2 hover:text-fg hover:bg-surface-2",
  danger: "bg-surface text-danger border border-border hover:border-danger/40 hover:bg-danger-soft",
};

const sizes: Record<Size, string> = {
  sm: "h-8 px-3 text-[13px] rounded-[10px]",
  md: "h-9 px-4 text-[13.5px] rounded-[var(--radius-control)]",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  loading?: boolean;
  icon?: ReactNode;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", loading, icon, className, children, disabled, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type="button"
      className={cn(base, variants[variant], sizes[size], className)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading ? <Loader2 className="size-4 anim-spin" aria-hidden /> : icon}
      {children}
    </button>
  );
});

export const IconButton = forwardRef<HTMLButtonElement, ButtonProps & { label: string }>(function IconButton(
  { label, className, size = "md", variant = "ghost", children, ...rest },
  ref,
) {
  return (
    <Tooltip content={label}>
      <button
        ref={ref}
        type="button"
        aria-label={label}
        className={cn(
          base,
          variants[variant],
          size === "sm" ? "size-8 rounded-[10px]" : "size-9 rounded-[var(--radius-control)]",
          className,
        )}
        {...rest}
      >
        {children}
      </button>
    </Tooltip>
  );
});

export function Card({ className, children, ...rest }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("rounded-[var(--radius-card)] border border-border bg-surface shadow-[var(--shadow-card)]", className)}
      {...rest}
    >
      {children}
    </div>
  );
}

type Tone = "neutral" | "ok" | "warn" | "danger" | "outline";

const tones: Record<Tone, string> = {
  neutral: "bg-surface-2 text-fg-2",
  ok: "bg-ok-soft text-ok",
  warn: "bg-warn-soft text-warn",
  danger: "bg-danger-soft text-danger",
  outline: "border border-border text-fg-2",
};

export function Badge({ tone = "neutral", dot, pulse, className, children }: {
  tone?: Tone;
  dot?: boolean;
  pulse?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <span
      className={cn(
        "inline-flex h-6 items-center gap-1.5 rounded-[var(--radius-chip)] px-2 text-[12px] font-medium leading-none",
        tones[tone],
        className,
      )}
    >
      {dot && <span className={cn("size-1.5 rounded-full bg-current", pulse && "anim-pulse")} aria-hidden />}
      {children}
    </span>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cn("size-4 anim-spin text-fg-3", className)} aria-hidden />;
}

export function Label({ htmlFor, children, className }: { htmlFor?: string; children: ReactNode; className?: string }) {
  return (
    <label htmlFor={htmlFor} className={cn("text-[13px] font-medium text-fg", className)}>
      {children}
    </label>
  );
}

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement> & { invalid?: boolean; mono?: boolean }>(
  function Input({ className, invalid, mono, ...rest }, ref) {
    return (
      <input
        ref={ref}
        className={cn(
          "h-10 w-full rounded-[var(--radius-control)] border bg-surface px-3 text-[14px] text-fg",
          "placeholder:text-fg-3 transition-[border-color,box-shadow] duration-150 outline-none",
          "focus:border-border-strong focus:shadow-[0_0_0_3px_var(--ring-soft)] disabled:opacity-50",
          invalid ? "border-danger/60" : "border-border hover:border-border-strong",
          mono && "font-mono text-[13px]",
          className,
        )}
        aria-invalid={invalid || undefined}
        spellCheck={false}
        autoComplete="off"
        {...rest}
      />
    );
  },
);

export function Field({ label, hint, error, children, htmlFor }: {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
      {error ? (
        <p className="text-[12.5px] text-danger" role="alert">
          {error}
        </p>
      ) : hint ? (
        <p className="text-[12.5px] text-fg-3">{hint}</p>
      ) : null}
    </div>
  );
}

export function Switch({ checked, onCheckedChange, label, description, disabled, id }: {
  checked: boolean;
  onCheckedChange: (v: boolean) => void;
  label: string;
  description?: ReactNode;
  disabled?: boolean;
  id?: string;
}) {
  const auto = useId();
  const switchId = id ?? auto;
  return (
    <div className="flex items-start justify-between gap-6 py-3">
      <div className="min-w-0">
        <label htmlFor={switchId} className="text-[13.5px] font-medium text-fg">
          {label}
        </label>
        {description && <p className="mt-0.5 text-[12.5px] leading-relaxed text-fg-3">{description}</p>}
      </div>
      <RadixSwitch.Root
        id={switchId}
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        className={cn(
          "relative mt-0.5 h-[22px] w-[38px] shrink-0 rounded-full border border-transparent transition-colors duration-150",
          "bg-surface-3 data-[state=checked]:bg-accent disabled:opacity-50",
        )}
      >
        <RadixSwitch.Thumb
          className={cn(
            "block size-[18px] translate-x-[1px] rounded-full bg-surface shadow-[0_1px_2px_rgb(0_0_0/0.25)]",
            "transition-transform duration-150 data-[state=checked]:translate-x-[17px] data-[state=checked]:bg-accent-fg",
          )}
        />
      </RadixSwitch.Root>
    </div>
  );
}

export function Segmented<T extends string>({ value, onChange, options, label, size = "md" }: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: string; icon?: ReactNode }[];
  label: string;
  size?: Size;
}) {
  const move = (dir: number) => {
    const i = options.findIndex((o) => o.value === value);
    const next = options[(i + dir + options.length) % options.length];
    if (next) onChange(next.value);
  };
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className="inline-flex rounded-[var(--radius-control)] border border-border bg-surface-2 p-[3px]"
      onKeyDown={(e) => {
        if (e.key === "ArrowRight" || e.key === "ArrowDown") {
          e.preventDefault();
          move(1);
        } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
          e.preventDefault();
          move(-1);
        }
      }}
    >
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={active}
            tabIndex={active ? 0 : -1}
            onClick={() => onChange(o.value)}
            className={cn(
              "inline-flex items-center gap-1.5 rounded-[9px] font-medium transition-[background-color,color,box-shadow] duration-150",
              size === "sm" ? "h-7 px-2.5 text-[12.5px]" : "h-8 px-3 text-[13px]",
              active ? "bg-raised text-fg shadow-[0_1px_2px_rgb(0_0_0/0.08)]" : "text-fg-2 hover:text-fg",
            )}
          >
            {o.icon}
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

export function Tooltip({ content, children }: { content: ReactNode; children: ReactNode }) {
  return (
    <RadixTooltip.Root delayDuration={400}>
      <RadixTooltip.Trigger asChild>{children}</RadixTooltip.Trigger>
      <RadixTooltip.Portal>
        <RadixTooltip.Content
          sideOffset={6}
          className="anim-fade z-50 rounded-[8px] bg-accent px-2 py-1 text-[12px] font-medium text-accent-fg shadow-[var(--shadow-pop)]"
        >
          {content}
        </RadixTooltip.Content>
      </RadixTooltip.Portal>
    </RadixTooltip.Root>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-5 min-w-5 items-center justify-center rounded-[6px] border border-border bg-surface-2 px-1 font-mono text-[11px] text-fg-3">
      {children}
    </kbd>
  );
}

export function SectionTitle({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="mb-2 flex items-center justify-between">
      <h2 className="text-[12px] font-semibold uppercase tracking-[0.06em] text-fg-3">{children}</h2>
      {action}
    </div>
  );
}
