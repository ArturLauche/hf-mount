import * as RadixDialog from "@radix-ui/react-dialog";
import * as RadixMenu from "@radix-ui/react-dropdown-menu";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { Button, cn } from "./primitives";

export function Dialog({ open, onOpenChange, title, description, children, footer, width = 520 }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
}) {
  return (
    <RadixDialog.Root open={open} onOpenChange={onOpenChange}>
      <RadixDialog.Portal>
        <RadixDialog.Overlay className="anim-fade fixed inset-0 z-40 bg-[var(--overlay)]" />
        <RadixDialog.Content
          style={{ maxWidth: width }}
          className={cn(
            "anim-pop fixed left-1/2 top-1/2 z-50 flex max-h-[calc(100vh-48px)] w-[calc(100vw-48px)] -translate-x-1/2 -translate-y-1/2 flex-col",
            "rounded-[var(--radius-dialog)] border border-border bg-surface shadow-[var(--shadow-pop)] outline-none",
          )}
        >
          <div className="flex items-start justify-between gap-4 px-6 pt-5">
            <div className="min-w-0">
              <RadixDialog.Title className="text-[17px] font-semibold tracking-[-0.01em] text-fg">{title}</RadixDialog.Title>
              {description ? (
                <RadixDialog.Description className="mt-1 text-[13px] text-fg-2">{description}</RadixDialog.Description>
              ) : (
                <RadixDialog.Description className="sr-only">{title}</RadixDialog.Description>
              )}
            </div>
            <RadixDialog.Close asChild>
              <button
                type="button"
                aria-label="Close"
                className="-mr-2 grid size-8 place-items-center rounded-[10px] text-fg-3 transition-colors hover:bg-surface-2 hover:text-fg"
              >
                <X className="size-4" />
              </button>
            </RadixDialog.Close>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-2 pt-4">{children}</div>
          {footer && <div className="flex items-center justify-end gap-2 px-6 pb-5 pt-3">{footer}</div>}
        </RadixDialog.Content>
      </RadixDialog.Portal>
    </RadixDialog.Root>
  );
}

export function ConfirmDialog({ open, onOpenChange, title, body, confirmLabel, danger, onConfirm, busy }: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  body: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
}) {
  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      width={440}
      footer={
        <>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm} loading={busy} autoFocus>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <div className="text-[13.5px] leading-relaxed text-fg-2">{body}</div>
    </Dialog>
  );
}

export const Menu = RadixMenu.Root;
export const MenuTrigger = RadixMenu.Trigger;

export function MenuContent({ children }: { children: ReactNode }) {
  return (
    <RadixMenu.Portal>
      <RadixMenu.Content
        align="end"
        sideOffset={6}
        className="anim-pop z-50 min-w-[200px] rounded-[14px] border border-border bg-surface p-1.5 shadow-[var(--shadow-pop)]"
      >
        {children}
      </RadixMenu.Content>
    </RadixMenu.Portal>
  );
}

export function MenuItem({ onSelect, icon, children, danger, disabled }: {
  onSelect: () => void;
  icon?: ReactNode;
  children: ReactNode;
  danger?: boolean;
  disabled?: boolean;
}) {
  return (
    <RadixMenu.Item
      disabled={disabled}
      onSelect={onSelect}
      className={cn(
        "flex h-8 cursor-default items-center gap-2.5 rounded-[9px] px-2.5 text-[13px] outline-none",
        "data-[highlighted]:bg-surface-2 data-[disabled]:opacity-40",
        danger ? "text-danger" : "text-fg",
      )}
    >
      <span className="text-fg-3 [&>svg]:size-4">{icon}</span>
      {children}
    </RadixMenu.Item>
  );
}

export function MenuSeparator() {
  return <RadixMenu.Separator className="my-1 h-px bg-border" />;
}

// ── Toasts ───────────────────────────────────────────────────────────

type ToastTone = "info" | "success" | "warning" | "error";
interface Toast {
  id: number;
  tone: ToastTone;
  title: string;
  body?: string;
  action?: { label: string; onClick: () => void };
}

const ToastContext = createContext<(t: Omit<Toast, "id">) => void>(() => {});

export function useToast() {
  return useContext(ToastContext);
}

const toastIcon: Record<ToastTone, ReactNode> = {
  info: <Info className="size-4 text-fg-2" />,
  success: <CheckCircle2 className="size-4 text-ok" />,
  warning: <AlertTriangle className="size-4 text-warn" />,
  error: <XCircle className="size-4 text-danger" />,
};

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const push = useCallback((t: Omit<Toast, "id">) => {
    const id = Date.now() + Math.random();
    setToasts((all) => [...all.slice(-3), { ...t, id }]);
    setTimeout(() => setToasts((all) => all.filter((x) => x.id !== id)), t.tone === "error" ? 9000 : 5000);
  }, []);
  const value = useMemo(() => push, [push]);
  return (
    <ToastContext.Provider value={value}>
      {children}
      <div
        className="pointer-events-none fixed bottom-5 right-5 z-[60] flex w-[360px] max-w-[calc(100vw-40px)] flex-col gap-2"
        role="status"
        aria-live="polite"
      >
        {toasts.map((t) => (
          <div
            key={t.id}
            className="anim-pop pointer-events-auto flex gap-3 rounded-[14px] border border-border bg-surface p-3.5 shadow-[var(--shadow-pop)]"
          >
            <div className="mt-0.5 shrink-0">{toastIcon[t.tone]}</div>
            <div className="min-w-0 flex-1">
              <p className="text-[13.5px] font-medium text-fg">{t.title}</p>
              {t.body && <p className="selectable mt-0.5 break-words text-[12.5px] text-fg-2">{t.body}</p>}
              {t.action && (
                <Button
                  size="sm"
                  className="mt-2"
                  onClick={() => {
                    t.action!.onClick();
                    setToasts((all) => all.filter((x) => x.id !== t.id));
                  }}
                >
                  {t.action.label}
                </Button>
              )}
            </div>
            <button
              type="button"
              aria-label="Dismiss notification"
              onClick={() => setToasts((all) => all.filter((x) => x.id !== t.id))}
              className="grid size-6 shrink-0 place-items-center rounded-[7px] text-fg-3 hover:bg-surface-2 hover:text-fg"
            >
              <X className="size-3.5" />
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
