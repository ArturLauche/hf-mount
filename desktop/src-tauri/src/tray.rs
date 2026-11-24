//! Tray icon: the app keeps running here when its window is closed. The menu
//! separates hiding the window, quitting the app (drives stay mounted) and
//! quitting with all drives unmounted.

use std::sync::atomic::Ordering;

use hf_mount::manager::protocol::{MountInfo, MountState};
use hf_mount::windows::{client, ops};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::{AppState, hide_main, navigate, quit, show_main, unmount_all_or_ask};

const TRAY_ID: &str = "main";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open hf-mount", true, None::<&str>)?;
    let new_drive = MenuItem::with_id(app, "new-drive", "New drive…", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", "Hide window", true, None::<&str>)?;
    let unmount_all = MenuItem::with_id(app, "unmount-all", "Unmount all drives", true, None::<&str>)?;
    let quit_keep = MenuItem::with_id(app, "quit", "Quit app (drives stay mounted)", true, None::<&str>)?;
    let quit_unmount = MenuItem::with_id(app, "quit-unmount", "Quit and unmount all drives", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &new_drive,
            &hide,
            &PredefinedMenuItem::separator(app)?,
            &unmount_all,
            &PredefinedMenuItem::separator(app)?,
            &quit_keep,
            &quit_unmount,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("hf-mount")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "new-drive" => navigate(app, "new-drive"),
            "hide" => {
                let _ = hide_main(app);
            }
            "unmount-all" => {
                let app = app.clone();
                // Drives with files open for writing are not forced; the
                // window asks about them.
                tauri::async_runtime::spawn(async move { unmount_all_or_ask(&app, false, false).await });
            }
            "quit" => quit(app, Some(false), false),
            "quit-unmount" => quit(app, Some(true), false),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(event, TrayIconEvent::Enter { .. }) {
                refresh_on_hover(tray.app_handle());
            } else if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

pub fn set_status(app: &AppHandle, text: &str) {
    let state = app.state::<AppState>();
    let mut previous = state.tooltip.lock().unwrap_or_else(|e| e.into_inner());
    if *previous != text
        && let Some(tray) = app.tray_by_id(TRAY_ID)
        && tray.set_tooltip(Some(text)).is_ok()
    {
        text.clone_into(&mut previous);
    }
}

/// Visible overview reads already feed the tooltip. With the window hidden,
/// only someone inspecting the tray needs a live list (including CLI hosts).
fn refresh_on_hover(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.window.visible.load(Ordering::Acquire) || state.quitting.load(Ordering::Acquire) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let Ok(_read) = state.status_read.try_lock() else {
            return;
        };
        if state.window.visible.load(Ordering::Acquire) {
            return;
        }
        // A stalled host should not hold up the next window restore for
        // the full control-request timeout just to populate a tooltip.
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client::list_hosts_with_error(&state.identity),
        )
        .await
        {
            Ok((hosts, error)) => update_status(
                &app,
                ops::driver_status().installed,
                hosts.iter().flat_map(|h| &h.mounts),
                error.is_some(),
            ),
            Err(_) if !state.quitting.load(Ordering::Acquire) => {
                set_status(&app, "hf-mount — mount status unavailable")
            }
            Err(_) => {}
        }
    });
}

pub(crate) fn update_status<'a>(
    app: &AppHandle,
    installed: bool,
    mounts: impl Iterator<Item = &'a MountInfo>,
    incomplete: bool,
) {
    if app.state::<AppState>().quitting.load(Ordering::Acquire) {
        return;
    }
    if !installed {
        set_status(app, "hf-mount — WinFsp is not installed");
        return;
    }
    let mut mounted = Vec::new();
    let mut failed = 0;
    let mut failed_uploads = 0;
    for mount in mounts {
        match mount.state {
            MountState::Mounted => mounted.push(mount.spec.target.as_str()),
            MountState::Failed => failed += 1,
            _ => {}
        }
        failed_uploads += mount.stats.as_ref().map_or(0, |s| s.failed_uploads);
    }
    mounted.sort_unstable();
    let mut text = match mounted.len() {
        0 if incomplete => "hf-mount — mount status unavailable".to_string(),
        0 => "hf-mount — no drives mounted".to_string(),
        1 => format!("hf-mount — {} mounted", mounted[0]),
        n => format!("hf-mount — {n} drives mounted ({})", mounted.join(" ")),
    };
    if incomplete && !mounted.is_empty() {
        text.push_str(", some hosts unavailable");
    }
    if failed > 0 {
        text.push_str(&format!(", {failed} failed"));
    }
    if failed_uploads > 0 {
        text.push_str(&format!(", {failed_uploads} failed uploads"));
    }
    set_status(app, &text);
}
