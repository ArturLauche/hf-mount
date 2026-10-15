import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
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

const POLL_VISIBLE_MS = 2000;
const POLL_HIDDEN_MS = 10000;

export function AppStateProvider({ children }: { children: ReactNode }) {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [overviewError, setOverviewError] = useState<ErrorInfo | null>(null);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [account, setAccount] = useState<AccountStatus | null>(null);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const inFlight = useRef<Promise<void> | null>(null);
  const again = useRef(false);
  const savedSettings = useRef<Settings | null>(null);
  const settingsQueue = useRef<Promise<unknown>>(Promise.resolve());

  // One request at a time. A refresh asked for while one is running (a poll
  // tick racing a button) is not dropped: it runs once more afterwards, and
  // every caller's promise resolves only after that fresh result is in.
  const refresh = useCallback((): Promise<void> => {
    if (inFlight.current) {
      again.current = true;
      return inFlight.current;
    }
    const run = async () => {
      do {
        again.current = false;
        try {
          setOverview(await api.overview());
          setOverviewError(null);
        } catch (e) {
          setOverviewError(errorInfo(e));
        }
      } while (again.current);
      inFlight.current = null;
    };
    inFlight.current = run();
    return inFlight.current;
  }, []);

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
    void refresh();
    void refreshProfiles();
    void refreshAccount(true);
  }, [refresh, refreshProfiles, refreshAccount]);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    const tick = () => {
      void refresh();
      timer = setTimeout(tick, document.visibilityState === "visible" ? POLL_VISIBLE_MS : POLL_HIDDEN_MS);
    };
    timer = setTimeout(tick, POLL_VISIBLE_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [refresh]);

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
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
