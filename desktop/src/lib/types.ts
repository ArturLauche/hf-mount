// Mirrors the Rust types in src/manager/{spec,protocol,config,auth}.rs and
// desktop/src-tauri/src/commands.rs. Field names are the serde names.

export type SourceType = "model" | "dataset" | "space" | "bucket";

export interface MountTuning {
  cache_dir?: string | null;
  cache_size_bytes?: number | null;
  no_disk_cache?: boolean;
  metadata_ttl_ms?: number | null;
  poll_interval_secs?: number | null;
  hub_endpoint?: string | null;
}

export interface MountSpec {
  source_type: SourceType;
  id: string;
  subfolder?: string | null;
  revision?: string | null;
  target: string;
  read_only: boolean;
  tuning?: MountTuning;
}

export type MountState = "starting" | "mounted" | "stopping" | "failed";

export type ErrorCode =
  | "invalid_argument"
  | "driver_missing"
  | "driver_unavailable"
  | "target_in_use"
  | "already_mounted"
  | "not_found"
  | "auth_required"
  | "invalid_token"
  | "forbidden"
  | "network"
  | "rate_limited"
  | "timeout"
  | "not_mounted"
  | "busy_with_open_files"
  | "uploads_incomplete"
  | "host_unavailable"
  | "protocol_mismatch"
  | "unsupported"
  | "internal";

export interface ErrorInfo {
  code: ErrorCode;
  message: string;
  hint?: string | null;
}

export interface MountStats {
  open_handles: number;
  open_write_handles: number;
  bytes_read: number;
  bytes_written: number;
  failed_operations: number;
  pending_uploads: number;
  pending_deletes: number;
  failed_uploads: number;
  last_error?: string | null;
}

export type HostKind = "supervisor" | "foreground";

export interface HostInfo {
  pid: number;
  kind: HostKind;
  version: string;
  protocol: number;
  started_at: string;
  elevated: boolean;
  winfsp_version?: string | null;
}

export interface MountInfo {
  id: string;
  spec: MountSpec;
  state: MountState;
  resolved_source?: string | null;
  credential_source?: string | null;
  host_pid: number;
  host_kind: HostKind;
  requested_at: string;
  mounted_at?: string | null;
  error?: ErrorInfo | null;
  stats?: MountStats | null;
}

export interface UnmountReport {
  target: string;
  unflushed_files: number;
  /** Deletions not committed: those files are still on the Hub. */
  unfinished_deletes?: number;
  recovered_to?: string | null;
  /** Where data that could not be copied to `recovered_to` was left. */
  kept_at?: string | null;
  warnings?: string[];
}

/** Drives a bulk unmount left mounted because files are open for writing. */
export interface BlockedUnmount {
  drives: { target: string; message: string }[];
  /** The app was quitting; it quits once these are unmounted. */
  quit: boolean;
}

export type EventLevel = "info" | "warning" | "error";

export interface ActivityEvent {
  time: string;
  level: EventLevel;
  target?: string | null;
  message: string;
}

export type Theme = "system" | "light" | "dark";

export interface Settings {
  theme: Theme;
  cache_dir: string | null;
  cache_size_bytes: number;
  upload_wait_secs: number;
  metadata_ttl_ms: number;
  launch_at_login: boolean;
  reconnect_on_start: boolean;
  close_to_tray: boolean;
  unmount_on_quit: boolean;
  hub_endpoint: string | null;
}

export interface Profile {
  id: string;
  name: string;
  spec: MountSpec;
  auto_mount: boolean;
  use_stored_token: boolean;
  created_at: string;
  last_mounted_at?: string | null;
}

export interface Account {
  name: string;
  fullname?: string | null;
  avatar_url?: string | null;
  token_name?: string | null;
  token_role?: string | null;
  orgs: string[];
}

export type AccountState = "signed_out" | "signed_in" | "invalid_token" | "unverified" | "stored";

export interface AccountStatus {
  state: AccountState;
  account?: Account | null;
  stored_account?: string | null;
  environment_variable?: string | null;
  error?: ErrorInfo | null;
}

export interface DriverStatus {
  installed: boolean;
  version?: string | null;
  install_dir?: string | null;
  error?: string | null;
  /** The installer ships the official WinFsp MSI and can launch it. */
  bundled_installer: boolean;
}

export interface LostMount {
  target: string;
  source: string;
}

export interface LostSession {
  pid: number;
  started_at: string;
  mounts: LostMount[];
}

export interface Overview {
  driver: DriverStatus;
  supervisor: HostInfo | null;
  mounts: MountInfo[];
  lost: LostSession | null;
  elevated: boolean;
  free_letters: string[];
}

export interface AppInfo {
  version: string;
  cli_path: string | null;
  home_dir: string;
  logs_dir: string;
  cache_dir: string;
  platform: string;
}

export type PathKind = "logs" | "cache" | "recovered" | "home";
