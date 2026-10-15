import { invoke } from "@tauri-apps/api/core";
import type {
  AccountStatus,
  ActivityEvent,
  AppInfo,
  ErrorInfo,
  MountInfo,
  MountSpec,
  Overview,
  PathKind,
  Profile,
  Settings,
  UnmountReport,
} from "./types";

/** True inside the desktop app; false in a plain browser (UI previews). */
export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** An error reported by the hf-mount engine, with a stable code. */
export class ApiError extends Error {
  readonly info: ErrorInfo;
  constructor(info: ErrorInfo) {
    super(info.message);
    this.info = info;
  }
}

function toApiError(e: unknown): ApiError {
  if (e instanceof ApiError) return e;
  if (e && typeof e === "object" && "code" in e && "message" in e) {
    return new ApiError(e as ErrorInfo);
  }
  return new ApiError({ code: "internal", message: String(e) });
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    if (!isTauri) {
      // Browser-only preview (development and screenshots); never loaded in the app.
      const { previewBackend } = await import("./preview");
      return (await previewBackend.call(cmd, args)) as T;
    }
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw toApiError(e);
  }
}

export const api = {
  overview: () => call<Overview>("overview"),
  startMount: (spec: MountSpec, useStoredToken: boolean) =>
    call<MountInfo>("start_mount", { spec, useStoredToken }),
  stopMount: (target: string, force: boolean) => call<UnmountReport>("stop_mount", { target, force }),
  dismissMount: (target: string) => call<void>("dismiss_mount", { target }),
  dismissLost: () => call<void>("dismiss_lost"),
  retryUploads: (target: string) => call<void>("retry_uploads", { target }),
  activity: (limit: number) => call<ActivityEvent[]>("activity", { limit }),
  logTail: (lines: number) => call<string[]>("log_tail", { lines }),
  diagnostics: () => call<string>("diagnostics"),
  accountStatus: (validate: boolean) => call<AccountStatus>("account_status", { validate }),
  signIn: (token: string) => call<AccountStatus>("sign_in", { token }),
  signOut: () => call<void>("sign_out"),
  settings: () => call<Settings>("get_settings"),
  saveSettings: (settings: Settings) => call<Settings>("save_settings", { settings }),
  profiles: () => call<Profile[]>("get_profiles"),
  saveProfile: (profile: Profile) => call<Profile[]>("save_profile", { profile }),
  deleteProfile: (id: string) => call<Profile[]>("delete_profile", { id }),
  validateSpec: (spec: MountSpec) => call<MountSpec>("validate_spec", { spec }),
  openDrive: (target: string) => call<void>("open_drive", { target }),
  openPath: (kind: PathKind) => call<void>("open_path", { kind }),
  installDriver: () => call<void>("install_driver"),
  appInfo: () => call<AppInfo>("app_info"),
  quit: (unmountAll: boolean) => call<void>("quit_app", { unmountAll }),
};

export function errorInfo(e: unknown): ErrorInfo {
  return toApiError(e).info;
}
