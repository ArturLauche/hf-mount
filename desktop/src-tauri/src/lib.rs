//! The hf-mount desktop app: a window and a tray icon on top of the same
//! per-user mount supervisor that `hf-mount.exe` uses. The app never hosts
//! drives itself; it asks the supervisor (started from the bundled
//! `hf-mount.exe`) to mount and unmount, so drives keep working when the
//! window or the whole app is closed, and the CLI sees the same drives.

mod commands;
mod tray;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use hf_mount::manager::config;
use hf_mount::manager::protocol::ErrorCode;
use hf_mount::windows::client::LostSession;
use hf_mount::windows::ops;
use hf_mount::windows::pipe::Identity;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

pub(crate) struct AppState {
    identity: Identity,
    /// `hf-mount.exe` installed next to the app; it runs the supervisor.
    cli_exe: Option<PathBuf>,
    /// A supervisor journal left by a crash, shown until dismissed.
    lost: Mutex<Option<LostSession>>,
    quitting: AtomicBool,
}

impl AppState {
    fn new() -> Result<Self, String> {
        let identity = Identity::current().map_err(|e| format!("cannot determine the current Windows user: {e}"))?;
        Ok(Self {
            identity,
            cli_exe: find_cli(),
            lost: Mutex::new(None),
            quitting: AtomicBool::new(false),
        })
    }
}

/// The CLI is installed next to the app (Tauri sidecar). During development
/// `HF_MOUNT_CLI` may point at a locally built `hf-mount.exe`.
fn find_cli() -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(p) = std::env::var_os("HF_MOUNT_CLI").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("hf-mount.exe");
    candidate.is_file().then_some(candidate)
}

pub(crate) fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Ask the window to switch page (`drives`, `new-drive`, ...).
pub(crate) fn navigate(app: &AppHandle, page: &str) {
    show_main(app);
    let _ = app.emit("navigate", page);
}

/// Drives that a bulk unmount left mounted because files are open for
/// writing; the window asks whether to unmount them anyway.
#[derive(Clone, serde::Serialize)]
struct BlockedUnmount {
    drives: Vec<BlockedDrive>,
    /// The app was quitting and quits once these are unmounted.
    quit: bool,
}

#[derive(Clone, serde::Serialize)]
struct BlockedDrive {
    target: String,
    message: String,
}

/// Unmount every drive without cutting off applications that are writing:
/// drives with files open for writing stay mounted, and the window asks
/// whether to force them (as the drives screen does for a single drive).
/// Returns `false` when drives were left mounted for that question (other
/// errors are logged).
pub(crate) async fn unmount_all_or_ask(app: &AppHandle, force: bool, quitting: bool) -> bool {
    let identity = app.state::<AppState>().identity.clone();
    let (_, errors) = ops::unmount_all(&identity, force).await;
    let mut drives = Vec::new();
    let mut failed = false;
    for (target, e) in errors {
        if e.code == ErrorCode::BusyWithOpenFiles {
            drives.push(BlockedDrive {
                target,
                message: e.message,
            });
        } else {
            tracing::warn!("unmounting {target}: {e}");
            failed = true;
        }
    }
    if drives.is_empty() {
        if failed && !quitting {
            // The drives screen shows what could not be unmounted.
            navigate(app, "drives");
        }
        return true;
    }
    navigate(app, "drives");
    let _ = app.emit("unmount-blocked", BlockedUnmount { drives, quit: quitting });
    false
}

/// Quit the app. Drives keep running in the supervisor unless `unmount`
/// (or the "unmount on quit" setting when `None`) asks to stop them first.
/// Drives with files open for writing are only unmounted with `force`;
/// otherwise the app stays open and asks.
pub(crate) fn quit(app: &AppHandle, unmount: Option<bool>, force: bool) {
    let state = app.state::<AppState>();
    if state.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    let unmount = unmount.unwrap_or_else(|| config::load_settings().unwrap_or_default().unmount_on_quit);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if unmount {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.hide();
            }
            tray::set_status(&app, "hf-mount — unmounting drives…");
            if !unmount_all_or_ask(&app, force, true).await {
                tray::set_status(&app, "hf-mount — some drives are still in use");
                app.state::<AppState>().quitting.store(false, Ordering::SeqCst);
                return;
            }
        }
        app.exit(0);
    });
}

/// Mount saved drives marked "mount at startup" that are not mounted yet.
async fn reconnect_saved_drives(app: AppHandle) {
    let settings = config::load_settings().unwrap_or_default();
    if !settings.reconnect_on_start {
        return;
    }
    let Ok(profiles) = config::load_profiles() else {
        return;
    };
    let state = app.state::<AppState>();
    let Some(cli) = state.cli_exe.clone() else {
        return;
    };
    let overview = ops::overview(&state.identity).await;
    if !overview.driver.installed {
        return;
    }
    for profile in profiles.into_iter().filter(|p| p.auto_mount) {
        if overview.mounts.iter().any(|m| m.spec.target == profile.spec.target) {
            continue;
        }
        // The same token choice as mounting by hand: the drive's own Hub.
        let credential = profile
            .use_stored_token
            .then(|| ops::stored_credential_for(&profile.spec))
            .flatten();
        match ops::mount(&state.identity, &cli, profile.spec.clone(), credential).await {
            // The mount can take minutes; the drive may have been edited or
            // deleted meanwhile, so only its "last mounted" time is updated.
            Ok(_) => match config::mark_profile_mounted(&profile.id, &commands::now_rfc3339()) {
                Ok(true) => {}
                Ok(false) => tracing::info!(
                    "{} was removed from the saved drives while mounting",
                    profile.spec.target
                ),
                Err(e) => tracing::warn!("saving the mount time of {}: {e}", profile.spec.target),
            },
            // The supervisor keeps failed mounts in its list, so the drives
            // screen shows the error with a "Try again" button.
            Err(e) => tracing::warn!("reconnecting {}: {e}", profile.spec.target),
        }
    }
}

pub fn run() {
    hf_mount::windows::host::init_file_logging("desktop.log");
    let state = match AppState::new() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("startup: {e}");
            std::process::exit(1);
        }
    };
    let minimized = std::env::args().any(|a| a == "--minimized");

    let app = tauri::Builder::default()
        // A second launch (Start menu, autostart) focuses the running app.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| show_main(app)))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::overview,
            commands::start_mount,
            commands::stop_mount,
            commands::dismiss_mount,
            commands::dismiss_lost,
            commands::retry_uploads,
            commands::activity,
            commands::log_tail,
            commands::diagnostics,
            commands::account_status,
            commands::sign_in,
            commands::sign_out,
            commands::get_settings,
            commands::save_settings,
            commands::get_profiles,
            commands::save_profile,
            commands::delete_profile,
            commands::mark_profile_mounted,
            commands::validate_spec,
            commands::open_drive,
            commands::open_path,
            commands::install_driver,
            commands::app_info,
            commands::quit_app,
        ])
        .setup(move |app| {
            tray::create(app.handle())?;
            if !minimized {
                show_main(app.handle());
            }
            tauri::async_runtime::spawn(reconnect_saved_drives(app.handle().clone()));
            tauri::async_runtime::spawn(tray::keep_status_fresh(app.handle().clone()));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if config::load_settings().unwrap_or_default().close_to_tray {
                    let _ = window.hide();
                } else {
                    quit(window.app_handle(), None, false);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the hf-mount desktop app");

    app.run(|app, event| {
        // Closing the last window must not end the app while it lives in the
        // tray; only `quit` (which calls `exit`) does.
        if let RunEvent::ExitRequested { api, code: None, .. } = event
            && !app.state::<AppState>().quitting.load(Ordering::SeqCst)
        {
            api.prevent_exit();
        }
    });
}
