import * as RadixTooltip from "@radix-ui/react-tooltip";
import { Activity, EyeOff, HardDrive, LogOut, Power, Settings as SettingsIcon, UserRound, type LucideIcon } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Logo } from "./components/common";
import { Menu, MenuContent, MenuItem, MenuSeparator, MenuTrigger, ToastProvider, useToast } from "./components/ui/overlay";
import { Badge, cn, Kbd, Tooltip } from "./components/ui/primitives";
import { api, errorInfo } from "./lib/api";
import { hideWindow, onAppEvent } from "./lib/native";
import { AppStateProvider, useApp } from "./lib/state";
import { AccountPage } from "./pages/Account";
import { ActivityPage } from "./pages/Activity";
import { DrivesPage } from "./pages/Drives";
import { SettingsPage } from "./pages/Settings";

export type Page = "drives" | "activity" | "account" | "settings";

const NAV: { page: Page; label: string; icon: LucideIcon; key: string }[] = [
  { page: "drives", label: "Drives", icon: HardDrive, key: "1" },
  { page: "activity", label: "Activity", icon: Activity, key: "2" },
  { page: "account", label: "Account", icon: UserRound, key: "3" },
  { page: "settings", label: "Settings", icon: SettingsIcon, key: "4" },
];

export function App() {
  return (
    <RadixTooltip.Provider delayDuration={400} skipDelayDuration={200}>
      <ToastProvider>
        <AppStateProvider>
          <Shell />
        </AppStateProvider>
      </ToastProvider>
    </RadixTooltip.Provider>
  );
}

function Shell() {
  const [page, setPage] = useState<Page>("drives");
  const [newDriveRequest, setNewDriveRequest] = useState(0);

  const requestNewDrive = useCallback(() => {
    setPage("drives");
    setNewDriveRequest((n) => n + 1);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      if (e.key.toLowerCase() === "n") {
        e.preventDefault();
        requestNewDrive();
        return;
      }
      const nav = NAV.find((n) => n.key === e.key);
      if (nav) {
        e.preventDefault();
        setPage(nav.page);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [requestNewDrive]);

  // Tray menu and second-instance launches ask the window to show a page.
  useEffect(
    () =>
      onAppEvent<string>("navigate", (target) => {
        if (target === "new-drive") requestNewDrive();
        else if (NAV.some((n) => n.page === target)) setPage(target as Page);
      }),
    [requestNewDrive],
  );

  return (
    <div className="flex h-full">
      <Sidebar page={page} onNavigate={setPage} />
      <main className="min-w-0 flex-1 overflow-y-auto" id="main">
        <div key={page} className="anim-fade mx-auto w-full max-w-[920px] px-8 pb-16 pt-9 max-[720px]:px-5">
          {page === "drives" && <DrivesPage newDriveRequest={newDriveRequest} onNavigate={setPage} />}
          {page === "activity" && <ActivityPage />}
          {page === "account" && <AccountPage />}
          {page === "settings" && <SettingsPage />}
        </div>
      </main>
    </div>
  );
}

function Sidebar({ page, onNavigate }: { page: Page; onNavigate: (p: Page) => void }) {
  const { overview, account, preview } = useApp();
  const mounted = overview?.mounts.filter((m) => m.state === "mounted").length ?? 0;
  const failed = overview?.mounts.filter((m) => m.state === "failed").length ?? 0;

  return (
    <aside className="flex w-[232px] shrink-0 flex-col border-r border-border bg-bg px-3 pb-3 pt-4 max-[720px]:w-[68px]" aria-label="Sidebar">
      <div className="flex items-center gap-2.5 px-2.5 pb-5">
        <Logo className="size-[26px] shrink-0" />
        <div className="min-w-0 max-[720px]:hidden">
          <p className="text-[14.5px] font-semibold leading-tight tracking-[-0.01em] text-fg">hf-mount</p>
          <p className="text-[11.5px] leading-tight text-fg-3">Hugging Face drives</p>
        </div>
      </div>

      <nav aria-label="Main" className="flex flex-col gap-0.5">
        {NAV.map(({ page: p, label, icon: Icon, key }) => {
          const active = p === page;
          return (
            <button
              key={p}
              type="button"
              onClick={() => onNavigate(p)}
              aria-current={active ? "page" : undefined}
              title={`${label} (Ctrl+${key})`}
              className={cn(
                "group flex h-9 items-center gap-2.5 rounded-[11px] px-2.5 text-[13.5px] font-medium transition-colors duration-150",
                active ? "bg-surface text-fg shadow-[var(--shadow-card)] ring-1 ring-border" : "text-fg-2 hover:bg-surface-2 hover:text-fg",
              )}
            >
              <Icon className={cn("size-[17px] shrink-0", active ? "text-fg" : "text-fg-3 group-hover:text-fg-2")} />
              <span className="flex-1 text-left max-[720px]:hidden">{label}</span>
              {p === "drives" && mounted > 0 && (
                <span className="text-[12px] tabular-nums text-fg-3 max-[720px]:hidden">{mounted}</span>
              )}
              {p === "drives" && failed > 0 && <span className="size-1.5 rounded-full bg-danger" aria-label={`${failed} failed`} />}
            </button>
          );
        })}
      </nav>

      <div className="mt-auto flex flex-col gap-2">
        {preview && (
          <Tooltip content="Running in a browser with sample data. Nothing is mounted.">
            <span className="self-start px-1 max-[720px]:hidden">
              <Badge tone="outline">Preview data</Badge>
            </span>
          </Tooltip>
        )}
        <EngineStatus />
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={() => onNavigate("account")}
            className="flex min-w-0 flex-1 items-center gap-2.5 rounded-[11px] px-2 py-1.5 text-left transition-colors hover:bg-surface-2"
          >
            <Avatar name={account?.account?.name ?? account?.stored_account ?? null} />
            <span className="min-w-0 max-[720px]:hidden">
              <span className="block truncate text-[13px] font-medium text-fg">
                {account?.account?.name ?? account?.stored_account ?? "Not signed in"}
              </span>
              <span className="block truncate text-[11.5px] text-fg-3">
                {account?.state === "signed_in"
                  ? "Hugging Face"
                  : account?.state === "invalid_token"
                    ? "Token expired"
                    : "Public repos only"}
              </span>
            </span>
          </button>
          <QuitMenu />
        </div>
      </div>
    </aside>
  );
}

export function Avatar({ name, size = 28 }: { name: string | null; size?: number }) {
  return (
    <span
      style={{ width: size, height: size, fontSize: size * 0.42 }}
      className="grid shrink-0 place-items-center rounded-full border border-border bg-surface-2 font-semibold uppercase text-fg-2"
      aria-hidden
    >
      {name ? name.charAt(0) : <UserRound className="size-[55%] text-fg-3" />}
    </span>
  );
}

function EngineStatus() {
  const { overview, overviewError } = useApp();
  let tone = "bg-fg-3";
  let text = "Checking…";
  if (overviewError) {
    tone = "bg-danger";
    text = "Mount service unreachable";
  } else if (overview && !overview.driver.installed) {
    tone = "bg-warn";
    text = "WinFsp not installed";
  } else if (overview?.supervisor) {
    tone = "bg-ok";
    text = `Running · pid ${overview.supervisor.pid}`;
  } else if (overview) {
    tone = "bg-fg-3";
    text = "Idle · starts on first mount";
  }
  return (
    <div className="flex items-center gap-2 px-2.5 text-[12px] text-fg-3 max-[720px]:justify-center" role="status">
      <span className={cn("size-1.5 shrink-0 rounded-full", tone)} aria-hidden />
      <span className="truncate max-[720px]:hidden">{text}</span>
    </div>
  );
}

function QuitMenu() {
  const toast = useToast();
  const { overview } = useApp();
  const active = overview?.mounts.filter((m) => m.state !== "failed").length ?? 0;
  const quit = async (unmountAll: boolean) => {
    try {
      await api.quit(unmountAll);
    } catch (e) {
      const info = errorInfo(e);
      toast({ tone: "error", title: "Could not quit", body: info.message });
    }
  };
  return (
    <Menu>
      <Tooltip content="Window and quit">
        <MenuTrigger asChild>
          <button
            type="button"
            aria-label="Window and quit options"
            className="grid size-8 shrink-0 place-items-center rounded-[10px] text-fg-3 transition-colors hover:bg-surface-2 hover:text-fg max-[720px]:hidden"
          >
            <Power className="size-4" />
          </button>
        </MenuTrigger>
      </Tooltip>
      <MenuContent>
        <MenuItem icon={<EyeOff />} onSelect={() => void hideWindow()}>
          Hide window
          <span className="ml-auto pl-4 text-[11.5px] text-fg-3">stays in tray</span>
        </MenuItem>
        <MenuSeparator />
        <MenuItem icon={<LogOut />} onSelect={() => void quit(false)}>
          Quit app, keep drives
        </MenuItem>
        <MenuItem icon={<Power />} danger onSelect={() => void quit(true)} disabled={active === 0}>
          Quit and unmount {active > 0 ? `${active} ${active === 1 ? "drive" : "drives"}` : "drives"}
        </MenuItem>
        <div className="px-2.5 pb-1 pt-1.5 text-[11.5px] leading-snug text-fg-3">
          Drives keep working after the app quits. <Kbd>Ctrl</Kbd> <Kbd>N</Kbd> new drive
        </div>
      </MenuContent>
    </Menu>
  );
}
