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
  updateSettings: (s: Settings) => Promise<void>;
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

  const updateSettings = useCallback(async (s: Settings) => {
    const saved = await api.saveSettings(s);
    setSettings(saved);
    applyTheme(saved.theme);
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        const s = await api.settings();
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
