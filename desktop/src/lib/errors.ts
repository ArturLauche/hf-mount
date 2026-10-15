import type { ErrorCode, ErrorInfo } from "./types";

/** What the UI offers next to an error. */
export type ErrorAction = "sign_in" | "install_driver" | "pick_letter" | "retry" | "force_unmount" | "none";

const titles: Record<ErrorCode, string> = {
  invalid_argument: "Check the details",
  driver_missing: "WinFsp is not installed",
  driver_unavailable: "WinFsp is not working",
  target_in_use: "Drive letter is in use",
  already_mounted: "Drive letter is in use",
  not_found: "Not found",
  auth_required: "Sign-in needed",
  invalid_token: "Token was rejected",
  forbidden: "No access",
  network: "Can't reach Hugging Face",
  rate_limited: "Too many requests",
  timeout: "Hugging Face took too long",
  not_mounted: "Not mounted",
  busy_with_open_files: "Files are still open",
  uploads_incomplete: "Uploads did not finish",
  host_unavailable: "Mount service is not responding",
  protocol_mismatch: "Version mismatch",
  unsupported: "Not supported",
  internal: "Something went wrong",
};

const actions: Partial<Record<ErrorCode, ErrorAction>> = {
  driver_missing: "install_driver",
  target_in_use: "pick_letter",
  already_mounted: "pick_letter",
  auth_required: "sign_in",
  invalid_token: "sign_in",
  forbidden: "sign_in",
  network: "retry",
  rate_limited: "retry",
  timeout: "retry",
  host_unavailable: "retry",
  busy_with_open_files: "force_unmount",
};

export function errorTitle(info: ErrorInfo): string {
  return titles[info.code] ?? titles.internal;
}

export function errorAction(info: ErrorInfo): ErrorAction {
  return actions[info.code] ?? "none";
}

/** Sentence-case a backend message for display. */
export function sentence(message: string): string {
  const t = message.trim();
  if (!t) return t;
  const s = t.charAt(0).toUpperCase() + t.slice(1);
  return /[.!?]$/.test(s) ? s : `${s}.`;
}
