//! Commands called by the web UI (`desktop/src/lib/api.ts`). Every error is
//! an [`ErrorInfo`] with a stable code; messages are redacted by the engine.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use hf_mount::manager::config::{self, Profile, Settings};
use hf_mount::manager::protocol::{ActivityEvent, ErrorCode, ErrorInfo, MountInfo, UnmountReport};
use hf_mount::manager::spec::{MountSpec, normalize_target};
use hf_mount::windows::client;
use hf_mount::windows::ops::{self, AccountStatus, Overview};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::AppState;

type CmdResult<T> = Result<T, ErrorInfo>;

pub(crate) fn now_rfc3339() -> String {
    hf_mount::manager::protocol::now_rfc3339()
}

fn internal(e: impl std::fmt::Display) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Internal, e.to_string())
}

fn invalid(e: impl std::fmt::Display) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string())
}

fn io_error(what: &str, e: std::io::Error) -> ErrorInfo {
    if e.kind() == std::io::ErrorKind::InvalidInput {
        invalid(e)
    } else {
        internal(format!("{what}: {e}"))
    }
}

/// The official WinFsp installer shipped with the app (see `tauri.winfsp.conf.json`).
const WINFSP_MSI: &str = "winfsp-2.1.25156.msi";
/// SHA-256 of that MSI, as pinned in `scripts/windows/install-winfsp.ps1`.
const WINFSP_MSI_SHA256: &str = "073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a";

fn bundled_winfsp(app: &AppHandle) -> Option<PathBuf> {
    let path = app.path().resource_dir().ok()?.join("winfsp").join(WINFSP_MSI);
    path.is_file().then_some(path)
}

#[tauri::command]
pub async fn overview(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Overview> {
    let mut ov = ops::overview(&state.identity).await;
    ov.driver.bundled_installer = bundled_winfsp(&app).is_some();
    let mut lost = state.lost.lock().unwrap_or_else(|e| e.into_inner());
    if lost.is_none() {
        *lost = client::take_lost_session(&state.identity);
    }
    ov.lost = lost.clone();
    Ok(ov)
}

fn cli_exe(state: &AppState) -> CmdResult<&Path> {
    state.cli_exe.as_deref().ok_or_else(|| {
        ErrorInfo::new(ErrorCode::HostUnavailable, "hf-mount.exe was not found next to the app")
            .with_hint("reinstall hf-mount")
    })
}

#[tauri::command]
pub async fn start_mount(state: State<'_, AppState>, spec: MountSpec, use_stored_token: bool) -> CmdResult<MountInfo> {
    let cli = cli_exe(&state)?.to_path_buf();
    let credential = if use_stored_token {
        let endpoint = spec.tuning.hub_endpoint.clone().unwrap_or_else(ops::endpoint);
        ops::stored_credential(&endpoint)
    } else {
        None
    };
    ops::mount(&state.identity, &cli, spec, credential).await
}

#[tauri::command]
pub async fn stop_mount(state: State<'_, AppState>, target: String, force: bool) -> CmdResult<UnmountReport> {
    let target = normalize_target(&target).map_err(invalid)?;
    ops::unmount(&state.identity, &target, force).await
}

#[tauri::command]
pub async fn dismiss_mount(state: State<'_, AppState>, target: String) -> CmdResult<()> {
    let target = normalize_target(&target).map_err(invalid)?;
    ops::dismiss(&state.identity, &target).await
}

#[tauri::command]
pub fn dismiss_lost(state: State<'_, AppState>) -> CmdResult<()> {
    // Keep a marker so the journal is not re-read; it was already removed.
    *state.lost.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(())
}

#[tauri::command]
pub async fn retry_uploads(state: State<'_, AppState>, target: String) -> CmdResult<()> {
    let target = normalize_target(&target).map_err(invalid)?;
    ops::retry_uploads(&state.identity, &target).await
}

#[tauri::command]
pub async fn activity(state: State<'_, AppState>, limit: usize) -> CmdResult<Vec<ActivityEvent>> {
    Ok(ops::activity(&state.identity, limit.clamp(1, 1000)).await)
}

#[tauri::command]
pub fn log_tail(lines: usize) -> CmdResult<Vec<String>> {
    let path = config::logs_dir().join("supervisor.log");
    Ok(hf_mount::manager::logging::tail(&path, lines.clamp(1, 2000)))
}

#[tauri::command]
pub async fn diagnostics(app: AppHandle, state: State<'_, AppState>) -> CmdResult<String> {
    let version = app.package_info().version.to_string();
    Ok(ops::diagnostics(&state.identity, &version).await)
}

#[tauri::command]
pub async fn account_status(validate: bool) -> CmdResult<AccountStatus> {
    ops::account_status(&ops::endpoint(), validate).await
}

#[tauri::command]
pub async fn sign_in(token: String) -> CmdResult<AccountStatus> {
    ops::sign_in(&ops::endpoint(), &token).await
}

#[tauri::command]
pub fn sign_out() -> CmdResult<()> {
    ops::sign_out(&ops::endpoint())
}

#[tauri::command]
pub fn get_settings() -> CmdResult<Settings> {
    config::load_settings().map_err(|e| io_error("settings", e))
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> CmdResult<Settings> {
    config::save_settings(&settings).map_err(|e| io_error("settings", e))?;
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().unwrap_or(!settings.launch_at_login) != settings.launch_at_login {
        let result = if settings.launch_at_login {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        result.map_err(|e| internal(format!("could not change the sign-in startup entry: {e}")))?;
    }
    Ok(settings)
}

#[tauri::command]
pub fn get_profiles() -> CmdResult<Vec<Profile>> {
    config::load_profiles().map_err(|e| io_error("saved drives", e))
}

#[tauri::command]
pub fn save_profile(mut profile: Profile) -> CmdResult<Vec<Profile>> {
    profile.spec = profile.spec.validated().map_err(invalid)?;
    if profile.name.trim().is_empty() {
        profile.name = profile
            .spec
            .id
            .rsplit('/')
            .next()
            .unwrap_or(&profile.spec.id)
            .to_string();
    }
    config::upsert_profile(profile).map_err(|e| io_error("saved drives", e))
}

#[tauri::command]
pub fn delete_profile(id: String) -> CmdResult<Vec<Profile>> {
    config::delete_profile(&id).map_err(|e| io_error("saved drives", e))
}

#[tauri::command]
pub fn validate_spec(spec: MountSpec) -> CmdResult<MountSpec> {
    spec.validated().map_err(invalid)
}

/// Explorer with a validated drive root (`X:\`) as its only argument.
#[tauri::command]
pub fn open_drive(target: String) -> CmdResult<()> {
    let target = normalize_target(&target).map_err(invalid)?;
    open_in_explorer(Path::new(&format!("{target}\\")))
}

fn open_in_explorer(path: &Path) -> CmdResult<()> {
    let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    std::process::Command::new(Path::new(&windir).join("explorer.exe"))
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| internal(format!("could not open Explorer: {e}")))
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Logs,
    Cache,
    Recovered,
    Home,
}

#[tauri::command]
pub fn open_path(kind: PathKind) -> CmdResult<()> {
    let path = match kind {
        PathKind::Logs => config::logs_dir(),
        PathKind::Cache => config::load_settings().unwrap_or_default().cache_root(),
        PathKind::Recovered => config::recovered_dir(),
        PathKind::Home => config::home_dir(),
    };
    std::fs::create_dir_all(&path).map_err(|e| io_error("folder", e))?;
    open_in_explorer(&path)
}

/// Run the bundled, unmodified WinFsp installer with its normal UI. Windows
/// Installer asks for administrator approval itself (UAC); nothing is
/// installed silently. The file is checked against the pinned SHA-256 first.
#[tauri::command]
pub async fn install_driver(app: AppHandle) -> CmdResult<()> {
    let msi = bundled_winfsp(&app).ok_or_else(|| {
        ErrorInfo::new(
            ErrorCode::Unsupported,
            "this build does not include the WinFsp installer",
        )
        .with_hint("download it from https://winfsp.dev/rel/")
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&msi).map_err(|e| internal(format!("reading the WinFsp installer: {e}")))?;
        let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if digest != WINFSP_MSI_SHA256 {
            return Err(internal(
                "the bundled WinFsp installer failed its integrity check; reinstall hf-mount",
            ));
        }
        let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let status = std::process::Command::new(Path::new(&windir).join("System32").join("msiexec.exe"))
            .arg("/i")
            .arg(&msi)
            .arg("/norestart")
            .status()
            .map_err(|e| internal(format!("could not start Windows Installer: {e}")))?;
        match status.code() {
            Some(0) | Some(3010) => Ok(()),
            Some(1602) => Err(ErrorInfo::new(
                ErrorCode::DriverMissing,
                "the WinFsp installation was cancelled",
            )),
            Some(code) => Err(ErrorInfo::new(
                ErrorCode::DriverMissing,
                format!("the WinFsp installer failed (exit code {code})"),
            )),
            None => Err(internal("the WinFsp installer was interrupted")),
        }
    })
    .await
    .map_err(internal)?
}

#[derive(Debug, Serialize)]
pub struct AppInfo {
    version: String,
    cli_path: Option<PathBuf>,
    home_dir: PathBuf,
    logs_dir: PathBuf,
    cache_dir: PathBuf,
    platform: String,
}

#[tauri::command]
pub fn app_info(app: AppHandle, state: State<'_, AppState>) -> CmdResult<AppInfo> {
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        cli_path: state.cli_exe.clone(),
        home_dir: config::home_dir(),
        logs_dir: config::logs_dir(),
        cache_dir: config::load_settings().unwrap_or_default().cache_root(),
        platform: format!("windows {}", std::env::consts::ARCH),
    })
}

#[tauri::command]
pub fn quit_app(app: AppHandle, state: State<'_, AppState>, unmount_all: bool) -> CmdResult<()> {
    if state.quitting.load(Ordering::SeqCst) {
        return Ok(());
    }
    crate::quit(&app, Some(unmount_all));
    Ok(())
}
