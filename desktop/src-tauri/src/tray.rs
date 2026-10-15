//! Tray icon: the app keeps running here when its window is closed. The menu
//! separates hiding the window, quitting the app (drives stay mounted) and
//! quitting with all drives unmounted.

use std::time::Duration;

use hf_mount::manager::protocol::MountState;
use hf_mount::windows::ops;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::{AppState, navigate, quit, show_main, unmount_all_or_ask};

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
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
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
            if let TrayIconEvent::Click {
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
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(text));
    }
}

/// Keep the tray tooltip in sync with the mounted drives (including drives
/// mounted from the command line).
pub async fn keep_status_fresh(app: AppHandle) {
    let mut last = String::new();
    loop {
        let identity = app.state::<AppState>().identity.clone();
        let ov = ops::overview(&identity).await;
        let mounted: Vec<&str> = ov
            .mounts
            .iter()
            .filter(|m| m.state == MountState::Mounted)
            .map(|m| m.spec.target.as_str())
            .collect();
        let failed = ov.mounts.iter().filter(|m| m.state == MountState::Failed).count();
        let mut text = match mounted.len() {
            0 => "hf-mount — no drives mounted".to_string(),
            1 => format!("hf-mount — {} mounted", mounted[0]),
            n => format!("hf-mount — {n} drives mounted ({})", mounted.join(" ")),
        };
        if failed > 0 {
            text.push_str(&format!(", {failed} failed"));
        }
        if !ov.driver.installed {
            text = "hf-mount — WinFsp is not installed".to_string();
        }
        if text != last {
            set_status(&app, &text);
            last = text;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
