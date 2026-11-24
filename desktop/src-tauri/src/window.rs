//! The main window is created on demand on a tray-only launch. Navigation
//! and unmount confirmations may arrive before React has attached a listener.
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, WebviewWindow, WebviewWindowBuilder};

use crate::AppState;

pub(crate) struct MainWindow {
    pub visible: AtomicBool,
    opening: AtomicBool,
    /// Remove the key when its JS listener is ready. Only the latest pending
    /// destination/prompt matters, as for the corresponding React state.
    waiting: Mutex<HashMap<&'static str, Option<serde_json::Value>>>,
}

impl Default for MainWindow {
    fn default() -> Self {
        Self {
            visible: AtomicBool::new(false),
            opening: AtomicBool::new(false),
            waiting: Mutex::new(HashMap::from([("navigate", None), ("unmount-blocked", None)])),
        }
    }
}

fn reveal(window: &WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    publish_visibility(window.app_handle(), false);
}

pub(crate) fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        reveal(&window);
        return;
    }
    if app.state::<AppState>().window.opening.swap(true, Ordering::AcqRel) {
        return;
    }
    // WebView2 creation from a synchronous Windows event handler deadlocks.
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if let Some(config) = app.config().app.windows.iter().find(|w| w.label == "main") {
            match WebviewWindowBuilder::from_config(&app, config).and_then(|b| b.build()) {
                Ok(window) => reveal(&window),
                Err(e) => tracing::error!("opening the main window: {e}"),
            }
        }
        state.window.opening.store(false, Ordering::Release);
    });
}

pub(crate) fn hide_main(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide()?;
        publish_visibility(app, false);
    }
    Ok(())
}

fn set_memory_target(window: &WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL, ICoreWebView2_19,
    };
    use windows_core::Interface;

    let app = window.app_handle().clone();
    let _ = window.with_webview(move |view| {
        // SAFETY: Tauri runs this closure on the WebView2 UI thread. Older
        // runtimes can lack this optional interface and keep their defaults.
        unsafe {
            if let Ok(core) = view.controller().CoreWebView2()
                && let Ok(memory) = core.cast::<ICoreWebView2_19>()
            {
                let visible = app.state::<AppState>().window.visible.load(Ordering::Acquire);
                let level = if visible {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                } else {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                };
                let _ = memory.SetMemoryUsageTargetLevel(level);
            }
        }
    });
}

pub(crate) fn publish_visibility(app: &AppHandle, initial: bool) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // Tray commands and asynchronous window creation can race window events.
    // Sample and publish on the UI thread so an older observation cannot win.
    let _ = app.run_on_main_thread(move || update_visibility(&window, initial));
}

fn update_visibility(window: &WebviewWindow, initial: bool) {
    let app = window.app_handle();
    let visible = window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false);
    let state = app.state::<AppState>();
    let changed = state.window.visible.swap(visible, Ordering::AcqRel) != visible;
    if changed {
        set_memory_target(window);
    }
    if changed || initial {
        if !visible {
            if let Ok(mut reader) = state.status_read.try_lock() {
                reader.suspend();
            } else {
                // A status read may have observed the old visibility while
                // still holding the lock. Release its pipes once it finishes.
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    let mut reader = state.status_read.lock().await;
                    if !state.window.visible.load(Ordering::Acquire) {
                        reader.suspend();
                    }
                });
            }
        }
        let _ = window.emit("window-visibility", visible);
    }
}

pub(crate) fn send_when_ready<T: serde::Serialize + Clone>(app: &AppHandle, event: &'static str, payload: T) {
    let state = app.state::<AppState>();
    let mut waiting = state.window.waiting.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = waiting.get_mut(event) {
        *slot = serde_json::to_value(payload).ok();
    } else {
        let _ = app.emit(event, payload);
    }
}

#[tauri::command]
pub(crate) fn ui_listener_ready(app: AppHandle, event: String) {
    if event == "window-visibility" {
        publish_visibility(&app, true);
        return;
    }
    let state = app.state::<AppState>();
    let mut waiting = state.window.waiting.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Some(payload)) = waiting.remove(event.as_str()) {
        let _ = app.emit(&event, payload);
    }
}
