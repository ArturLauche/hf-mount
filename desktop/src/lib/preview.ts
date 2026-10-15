// In-memory stand-in for the Rust backend, used only when the UI runs in a
// plain browser (development previews and screenshots). The app shows a
// "Preview data" marker whenever this is active; it never runs inside the
// installed desktop app, where every call goes to the real engine.
//
// URL parameters select a scenario: ?state=empty|populated|nodriver|errors|elevated
// &account=signed_out|signed_in|invalid

import type {
  AccountStatus,
  ActivityEvent,
  AppInfo,
  ErrorInfo,
  MountInfo,
  MountSpec,
  Overview,
  Profile,
  Settings,
} from "./types";

const params = typeof window !== "undefined" ? new URLSearchParams(window.location.search) : new URLSearchParams();
const scenario = params.get("state") ?? "populated";
const accountScenario = params.get("account") ?? "signed_in";

const now = () => new Date().toISOString();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const fail = (info: ErrorInfo) => {
  throw info;
};

let settings: Settings = {
  theme: (params.get("theme") as Settings["theme"]) ?? "system",
  cache_dir: null,
  cache_size_bytes: 10_000_000_000,
  upload_wait_secs: 300,
  metadata_ttl_ms: 10_000,
  launch_at_login: false,
  reconnect_on_start: true,
  close_to_tray: true,
  unmount_on_quit: false,
  hub_endpoint: null,
};

function spec(source_type: MountSpec["source_type"], id: string, target: string, extra: Partial<MountSpec> = {}): MountSpec {
  return { source_type, id, target, read_only: source_type !== "bucket", subfolder: null, revision: null, ...extra };
}

function mount(s: MountSpec, extra: Partial<MountInfo> = {}): MountInfo {
  return {
    id: crypto.randomUUID(),
    spec: s,
    state: "mounted",
    resolved_source: `${s.source_type}/${s.id}${s.source_type === "bucket" ? "" : `/${s.revision ?? "main"}`}`,
    credential_source: s.source_type === "bucket" ? "Windows Credential Manager" : null,
    host_pid: 4128,
    host_kind: "supervisor",
    requested_at: now(),
    mounted_at: now(),
    stats: {
      open_handles: 0,
      open_write_handles: 0,
      bytes_read: 0,
      bytes_written: 0,
      failed_operations: 0,
      pending_uploads: 0,
      pending_deletes: 0,
      failed_uploads: 0,
    },
    ...extra,
  };
}

let mounts: MountInfo[] = [];
let profiles: Profile[] = [];
const events: ActivityEvent[] = [];

function record(level: ActivityEvent["level"], target: string | null, message: string) {
  events.push({ time: now(), level, target, message });
}

if (scenario === "populated" || scenario === "errors") {
  const gpt2 = spec("model", "openai-community/gpt2", "H:");
  const ds = spec("dataset", "HuggingFaceFW/fineweb-edu", "D:", { subfolder: "sample/10BT" });
  const bucket = spec("bucket", "alice/training-runs", "X:");
  mounts = [
    mount(gpt2, { stats: { ...mount(gpt2).stats!, open_handles: 2, bytes_read: 734_003_200 } }),
    mount(ds, { stats: { ...mount(ds).stats!, bytes_read: 18_874_368 } }),
    mount(bucket, {
      stats: { ...mount(bucket).stats!, open_handles: 1, open_write_handles: 1, bytes_written: 52_428_800, pending_uploads: 3 },
    }),
  ];
  profiles = [
    { id: "p-gpt2", name: "gpt2", spec: gpt2, auto_mount: true, use_stored_token: true, created_at: now() },
    { id: "p-fw", name: "fineweb-edu", spec: ds, auto_mount: false, use_stored_token: true, created_at: now() },
    { id: "p-runs", name: "training-runs", spec: bucket, auto_mount: true, use_stored_token: true, created_at: now() },
    {
      id: "p-llama",
      name: "Llama-3.2-1B",
      spec: spec("model", "meta-llama/Llama-3.2-1B", "L:"),
      auto_mount: false,
      use_stored_token: true,
      created_at: now(),
    },
  ];
  if (scenario === "errors") {
    const failed = spec("model", "meta-llama/Llama-3.2-1B", "L:");
    mounts.push(
      mount(failed, {
        state: "failed",
        stats: null,
        mounted_at: null,
        resolved_source: null,
        error: {
          code: "forbidden",
          message: "access to model meta-llama/Llama-3.2-1B@main is forbidden",
          hint: "for gated repositories, accept the terms on huggingface.co with the same account",
        },
      }),
    );
    mounts[2]!.stats = { ...mounts[2]!.stats!, failed_uploads: 1, last_error: "upload failed: network unreachable" };
  }
  record("info", null, "Supervisor started (pid 4128)");
  record("info", "H:", "Mounting model openai-community/gpt2@main (read-only)");
  record("info", "H:", "Mounted model/openai-community/gpt2/main at H:");
  record("info", "D:", "Mounted dataset/HuggingFaceFW/fineweb-edu/main/sample/10BT at D:");
  record("info", "X:", "Mounted bucket/alice/training-runs at X:");
  if (scenario === "errors") {
    record("error", "L:", "Mount failed: access to model meta-llama/Llama-3.2-1B@main is forbidden");
    record("warning", "X:", "upload of 1 file failed; it will be retried");
  }
}

const driverInstalled = scenario !== "nodriver";

let lost: Overview["lost"] =
  scenario === "errors"
    ? { pid: 3920, started_at: now(), mounts: [{ target: "R:", source: "dataset/HuggingFaceFW/fineweb/main" }] }
    : null;

function overview(): Overview {
  return {
    driver: driverInstalled
      ? { installed: true, version: "2.1", install_dir: "C:\\Program Files (x86)\\WinFsp\\", bundled_installer: true }
      : { installed: false, bundled_installer: true },
    supervisor:
      mounts.length > 0
        ? { pid: 4128, kind: "supervisor", version: "0.13.1", protocol: 1, started_at: now(), elevated: false, winfsp_version: "2.1" }
        : null,
    mounts,
    lost,
    elevated: scenario === "elevated",
    free_letters: "DEFGHIJKLMNOPQRSTUVWXYZ"
      .split("")
      .map((l) => `${l}:`)
      .filter((l) => !mounts.some((m) => m.spec.target === l && m.state !== "failed")),
  };
}

let account: AccountStatus =
  accountScenario === "signed_out"
    ? { state: "signed_out" }
    : accountScenario === "invalid"
      ? { state: "invalid_token", stored_account: "alice" }
      : {
          state: "signed_in",
          account: { name: "alice", fullname: "Alice Martin", token_name: "laptop", token_role: "write", orgs: ["acme-research"] },
          stored_account: "alice",
        };

const appInfo: AppInfo = {
  version: "0.13.1",
  cli_path: "C:\\Users\\alice\\AppData\\Local\\hf-mount Desktop\\hf-mount.exe",
  home_dir: "C:\\Users\\alice\\AppData\\Local\\hf-mount",
  logs_dir: "C:\\Users\\alice\\AppData\\Local\\hf-mount\\logs",
  cache_dir: "C:\\Users\\alice\\AppData\\Local\\hf-mount\\cache",
  platform: "windows (preview)",
};

export const previewBackend = {
  active: true,
  async call(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
    await sleep(80);
    switch (cmd) {
      case "overview":
        return overview();
      case "start_mount": {
        const s = args.spec as MountSpec;
        if (!driverInstalled) fail({ code: "driver_missing", message: "WinFsp is not installed" });
        if (mounts.some((m) => m.spec.target === s.target && m.state !== "failed"))
          fail({ code: "already_mounted", message: `${s.target} is already used by hf-mount` });
        await sleep(1200);
        if (s.id.includes("private"))
          fail({
            code: "auth_required",
            message: `${s.source_type} ${s.id}@main was not found, or it is private or gated`,
            hint: "sign in with a token that has access, or check the name",
          });
        mounts = mounts.filter((m) => m.spec.target !== s.target);
        const m = mount({ ...s, read_only: s.source_type !== "bucket" || s.read_only });
        mounts.push(m);
        record("info", s.target, `Mounted ${m.resolved_source} at ${s.target}`);
        return m;
      }
      case "stop_mount": {
        const target = args.target as string;
        const m = mounts.find((x) => x.spec.target === target);
        if (!m) fail({ code: "not_mounted", message: `nothing is mounted at ${target}` });
        if (m!.stats && m!.stats.open_write_handles > 0 && !args.force)
          fail({
            code: "busy_with_open_files",
            message: `${m!.stats.open_write_handles} file(s) on ${target} are open for writing`,
            hint: "close the applications using them, or unmount anyway",
          });
        await sleep(700);
        mounts = mounts.filter((x) => x !== m);
        record("info", target, "Unmounted");
        return { target, unflushed_files: 0, warnings: [] };
      }
      case "dismiss_mount":
        mounts = mounts.filter((x) => !(x.spec.target === args.target && x.state === "failed"));
        return null;
      case "retry_uploads":
        return null;
      case "dismiss_lost":
        lost = null;
        return null;
      case "activity":
        return events.slice(-(args.limit as number));
      case "log_tail":
        return events.map((e) => `${e.time}  ${e.level.toUpperCase()} hf_mount::activity: ${e.target ? `[${e.target}] ` : ""}${e.message}`);
      case "diagnostics":
        return "hf-mount diagnostics (preview data)\n";
      case "account_status":
        return account;
      case "sign_in":
        await sleep(600);
        if (!(args.token as string).startsWith("hf_"))
          fail({ code: "invalid_token", message: "the Hugging Face token is invalid or expired" });
        account = {
          state: "signed_in",
          account: { name: "alice", fullname: "Alice Martin", token_name: "laptop", token_role: "write", orgs: [] },
          stored_account: "alice",
        };
        return account;
      case "sign_out":
        account = { state: "signed_out" };
        return null;
      case "get_settings":
        return settings;
      case "save_settings":
        settings = args.settings as Settings;
        return settings;
      case "get_profiles":
        return profiles;
      case "save_profile": {
        const p = args.profile as Profile;
        profiles = [...profiles.filter((x) => x.id !== p.id), p];
        return profiles;
      }
      case "delete_profile":
        profiles = profiles.filter((x) => x.id !== args.id);
        return profiles;
      case "validate_spec":
        return args.spec;
      case "app_info":
        return appInfo;
      case "open_drive":
      case "open_path":
      case "install_driver":
      case "quit_app":
        return null;
      default:
        throw { code: "unsupported", message: `preview: ${cmd}` };
    }
  },
};
