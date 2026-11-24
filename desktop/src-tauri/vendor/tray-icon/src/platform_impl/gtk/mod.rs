// Copyright 2022-2022 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

mod icon;
use std::path::{Path, PathBuf};

use crate::icon::Icon;
pub(crate) use icon::PlatformIcon;

use crate::{TrayIconAttributes, TrayIconId};
use libappindicator::{AppIndicator, AppIndicatorStatus};

pub struct TrayIcon {
    id: TrayIconId,
    indicator: AppIndicator,
    temp_dir_path: Option<PathBuf>,
    path: Option<PathBuf>,
    counter: u32,
    menu: Option<Box<dyn muda::ContextMenu>>,
}

impl TrayIcon {
    pub fn new(id: TrayIconId, attrs: TrayIconAttributes) -> crate::Result<Self> {
        let mut indicator = AppIndicator::new(&format!("tray-icon tray app {}", id.as_ref()), "");

        if let Some(menu) = &attrs.menu {
            indicator.set_menu(&mut menu.gtk_context_menu());
        }

        if let Some(title) = attrs.title {
            indicator.set_label(title.as_str(), "");
        }

        let mut tray = Self {
            id,
            indicator,
            path: None,
            temp_dir_path: attrs.temp_dir_path,
            counter: 0,
            menu: attrs.menu,
        };
        tray.set_icon(attrs.icon)?;
        tray.indicator.set_status(AppIndicatorStatus::Active);
        Ok(tray)
    }
    pub fn set_icon(&mut self, icon: Option<Icon>) -> crate::Result<()> {
        let path = if let Some(icon) = icon {
            let counter = self.counter.wrapping_add(1);
            let (parent_path, icon_path) =
                temp_icon_path(self.temp_dir_path.as_deref(), &self.id, counter)?;
            icon.inner.write_to_png(&icon_path)?;
            self.indicator
                .set_icon_theme_path(&parent_path.to_string_lossy());
            self.indicator
                .set_icon_full(&icon_path.to_string_lossy(), "tray icon");
            self.counter = counter;
            Some(icon_path)
        } else {
            self.indicator.set_icon_full("", "tray icon");
            None
        };
        if let Some(previous) = std::mem::replace(&mut self.path, path) {
            let _ = std::fs::remove_file(previous);
        }
        Ok(())
    }

    pub fn set_menu(&mut self, menu: Option<Box<dyn crate::menu::ContextMenu>>) {
        if let Some(menu) = &menu {
            self.indicator.set_menu(&mut menu.gtk_context_menu());
        }
        self.menu = menu;
    }

    pub fn set_tooltip<S: AsRef<str>>(&mut self, _tooltip: Option<S>) -> crate::Result<()> {
        Ok(())
    }

    pub fn set_title<S: AsRef<str>>(&mut self, title: Option<S>) {
        self.indicator
            .set_label(title.as_ref().map(|t| t.as_ref()).unwrap_or(""), "");
    }

    pub fn set_visible(&mut self, visible: bool) -> crate::Result<()> {
        if visible {
            self.indicator.set_status(AppIndicatorStatus::Active);
        } else {
            self.indicator.set_status(AppIndicatorStatus::Passive);
        }

        Ok(())
    }

    pub fn set_temp_dir_path<P: AsRef<Path>>(&mut self, path: Option<P>) {
        self.temp_dir_path = path.map(|p| p.as_ref().to_path_buf());
    }

    pub fn rect(&self) -> Option<crate::Rect> {
        None
    }

    pub fn app_indicator(&self) -> &AppIndicator {
        &self.indicator
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.indicator.set_status(AppIndicatorStatus::Passive);
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Generates an icon path in one of the following dirs:
/// 1. If `temp_icon_dir` is `Some` use that.
/// 2. `$XDG_RUNTIME_DIR/tray-icon`
/// 3. The system temporary directory joined with `tray-icon`
fn temp_icon_path(
    temp_icon_dir: Option<&Path>,
    id: &TrayIconId,
    counter: u32,
) -> std::io::Result<(PathBuf, PathBuf)> {
    let parent_path = icon_dir(temp_icon_dir, dirs::runtime_dir, std::env::temp_dir);

    std::fs::create_dir_all(&parent_path)?;
    let icon_path = parent_path.join(format!("tray-icon-{}-{}.png", id.as_ref(), counter));
    Ok((parent_path, icon_path))
}

fn icon_dir(
    override_dir: Option<&Path>,
    runtime_dir: impl FnOnce() -> Option<PathBuf>,
    temp_dir: impl FnOnce() -> PathBuf,
) -> PathBuf {
    override_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| runtime_dir().unwrap_or_else(temp_dir).join("tray-icon"))
}

#[test]
fn temp_icon_path_preference_order() {
    let override_dir = Path::new("/override");
    let runtime_dir = || Some(PathBuf::from("/runtime"));
    let temp_dir = || PathBuf::from("/temp");
    assert_eq!(
        icon_dir(Some(override_dir), runtime_dir, temp_dir),
        override_dir
    );
    assert_eq!(
        icon_dir(None, runtime_dir, temp_dir),
        Path::new("/runtime/tray-icon")
    );
    assert_eq!(
        icon_dir(None, || None, temp_dir),
        Path::new("/temp/tray-icon")
    );
}
