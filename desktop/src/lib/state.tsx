import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { updateRelativeTimes } from "../hooks/useRelativeTime";
import { useVisiblePolling } from "../hooks/useVisiblePolling";
import { onAppEvent } from "./native";
import { sameSnapshot } from "./snapshot";
import { api, errorInfo, isTauri } from "./api";
import type { AccountStatus, AppInfo, ErrorInfo, Overview, Profile, Settings, Theme } from "./types";

interface AppState {
  overview: Overview | null;
  overviewError: ErrorInfo | null;
  profiles: Profile[];
  settings: Settings | null;
  account: AccountStatus | null;
  appInfo: AppInfo | null;
  preview: boolean;
  refresh: () => Promise<void>;
  refreshProfiles: () => Promise<void>;
  setProfiles: (p: Profile[]) => void;
  /** Save a change to some settings; changes are saved one after another. */
  updateSettings: (change: Partial<Settings>) => Promise<void>;
  refreshAccount: (validate: boolean) => Promise<void>;
  setAccount: (a: AccountStatus) => void;
}

const Ctx = createContext<AppState | null>(null);

export function useApp(): AppState {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useApp outside AppStateProvider");
  return ctx;
}

export function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset.theme;
  else root.dataset.theme = theme;
}

const Visibility = createContext(false);
const ActivityHosts = createContext<"unknown" | "available" | "empty" | "unavailable">("unknown");
export const useActivityHosts = () => useContext(ActivityHosts);
export const useWindowVisible = () => useContext(Visibility);

function useMainVisibility() {
  const [visible, setVisible] = useState(() => !isTauri && document.visibilityState === "visible");
  useEffect(() => {
    let nativeVisible = !isTauri;
    const update = () => setVisible(nativeVisible && document.visibilityState === "visible");
    const onVisibility = (value: boolean) => {
      nativeVisible = value;
      update();
    };
    const stop = onAppEvent<boolean>("window-visibility", onVisibility, () => onVisibility(true));
    document.addEventListener("visibilitychange", update);
    update();
    return () => {
      stop();
      document.removeEventListener("visibilitychange", update);
    };
  }, []);
  return visible;
}

export function AppStateProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<Pick<AppState, "overview" | "overviewError">>({ overview: null, overviewError: null });
  const { overview, overviewError } = status;
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [account, setAccount] = useState<AccountStatus | null>(null);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const visible = useMainVisibility();
  const lastStatus = useRef(status);
  const loaded = useRef(false);
  const savedSettings = useRef<Settings | null>(null);
  const settingsQueue = useRef<Promise<unknown>>(Promise.resolve());

  const readOverview = useCallback(async (signal: AbortSignal) => {
    updateRelativeTimes();
    let next: Overview | null;
    let error: ErrorInfo | null;
    try {
      next = await api.overview();
      error = next.status_error ?? null;
    } catch (e) {
      next = null;
      error = errorInfo(e);
    }
    if (signal.aborted) return false;
    const changed = !sameSnapshot(lastStatus.current.overview, next);
    if (changed || !sameSnapshot(lastStatus.current.overviewError, error)) {
      lastStatus.current = { overview: next, overviewError: error };
      setStatus(lastStatus.current);
    }
    return next !== null && (changed || next.mounts.some((m) => m.state === "starting" || m.state === "stopping"));
  }, []);
  const refresh = useVisiblePolling(visible, readOverview, 2000, 8000);

  const refreshProfiles = useCallback(async () => {
    try {
      setProfiles(await api.profiles());
    } catch {
      /* surfaced on the Drives page when saving */
    }
  }, []);

  const refreshAccount = useCallback(async (validate: boolean) => {
    try {
      setAccount(await api.accountStatus(validate));
    } catch (e) {
      setAccount({ state: "unverified", error: errorInfo(e) });
    }
  }, []);

  // Saves run one at a time, each applying only its own change to the
  // settings as last saved. Two controls changed before the first save
  // answers therefore both stick: the second save never carries the first
  // control's old value. A failed save changes nothing.
  const updateSettings = useCallback((change: Partial<Settings>): Promise<void> => {
    const save = async () => {
      const base = savedSettings.current;
      if (!base) throw new Error("the settings have not loaded yet");
      const saved = await api.saveSettings({ ...base, ...change });
      savedSettings.current = saved;
      setSettings(saved);
      applyTheme(saved.theme);
    };
    const result = settingsQueue.current.then(save, save);
    settingsQueue.current = result.catch(() => undefined);
    return result;
  }, []);

  useEffect(() => {
    if (!visible || loaded.current) return;
    loaded.current = true;
    void (async () => {
      try {
        const s = await api.settings();
        savedSettings.current = s;
        setSettings(s);
        applyTheme(s.theme);
      } catch {
        /* defaults stay */
      }
      try {
        setAppInfo(await api.appInfo());
      } catch {
        /* optional */
      }
    })();
    void refreshProfiles();
    void refreshAccount(true);
  }, [visible, refreshProfiles, refreshAccount]);

  const value = useMemo<AppState>(
    () => ({
      overview,
      overviewError,
      profiles,
      settings,
      account,
      appInfo,
      preview: !isTauri,
      refresh,
      refreshProfiles,
      setProfiles,
      updateSettings,
      refreshAccount,
      setAccount,
    }),
    [overview, overviewError, profiles, settings, account, appInfo, refresh, refreshProfiles, updateSettings, refreshAccount],
  );
  const activityHosts = overview?.has_hosts === true ? "available"
    : overviewError || overview?.foreground_unavailable ? "unavailable"
    : overview?.has_hosts === false ? "empty" : "unknown";
  return (
    <Visibility.Provider value={visible}>
      <ActivityHosts.Provider value={activityHosts}>
        <Ctx.Provider value={value}>{children}</Ctx.Provider>
      </ActivityHosts.Provider>
    </Visibility.Provider>
  );
}
