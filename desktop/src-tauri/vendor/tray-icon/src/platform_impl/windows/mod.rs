// Copyright 2022-2022 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

mod icon;
mod util;
use std::{
    cell::{Cell, RefCell},
    io,
    mem::size_of,
    ptr,
    rc::Rc,
};

use once_cell::sync::Lazy;
use windows_sys::{
    core::GUID,
    s,
    Win32::{
        Foundation::{
            GetLastError, SetLastError, HWND, LPARAM, LRESULT, POINT, RECT, S_OK, WPARAM,
        },
        UI::{
            Shell::{
                Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_GUID, NIF_ICON, NIF_MESSAGE,
                NIF_STATE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIS_HIDDEN, NOTIFYICONDATAW,
                NOTIFYICONIDENTIFIER,
            },
            WindowsAndMessaging::{
                ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                GetCursorPos, KillTimer, PostMessageW, RegisterClassW, RegisterWindowMessageA,
                SetForegroundWindow, SetTimer, TrackPopupMenu, CW_USEDEFAULT, GWL_USERDATA, HMENU,
                MSGFLT_ALLOW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
                WM_NCDESTROY, WM_NULL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER,
                WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
                WS_OVERLAPPED,
            },
        },
    },
};

use crate::{
    dpi::PhysicalPosition, icon::Icon, menu, MouseButton, MouseButtonState, Rect,
    TrayIconAttributes, TrayIconEvent, TrayIconId, COUNTER,
};

pub(crate) use self::icon::WinIcon as PlatformIcon;

const WM_USER_TRAYICON: u32 = 6002;
const LEAVE_TIMER_ID: usize = 6007;
const REGISTER_TIMER_ID: usize = 6010;
const INITIAL_REGISTER_RETRY_MS: u32 = 1_000;
const MAX_REGISTER_RETRY_MS: u32 = 30_000;
/// When the taskbar is created, it registers a message with the "TaskbarCreated" string and then broadcasts this message to all top-level windows
/// When the application receives this message, it should assume that any taskbar icons it added have been removed and add them again.
static S_U_TASKBAR_RESTART: Lazy<u32> =
    Lazy::new(|| unsafe { RegisterWindowMessageA(s!("TaskbarCreated")) });

struct TrayUserData {
    internal_id: u32,
    guid: Option<GUID>,
    id: TrayIconId,
    hpopupmenu: Cell<Option<HMENU>>,
    icon: RefCell<Option<Icon>>,
    tooltip: RefCell<Option<String>>,
    last_position: Cell<Option<PhysicalPosition<f64>>>,
    menu_on_left_click: Cell<bool>,
    menu_on_right_click: Cell<bool>,
    visible: Cell<bool>,
    next_retry_ms: Cell<u32>,
}

impl TrayUserData {
    fn is_attached(&self, hwnd: HWND) -> bool {
        unsafe { util::get_window_long(hwnd, GWL_USERDATA) == self as *const Self as isize }
    }

    fn notification(&self, hwnd: HWND, flags: u32) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            uFlags: flags,
            hWnd: hwnd,
            uID: self.internal_id,
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            ..Default::default()
        };
        apply_guid(&mut nid, self.guid);
        nid
    }

    fn cancel_registration_retry(&self, hwnd: HWND) {
        if self.next_retry_ms.replace(0) != 0 {
            unsafe { KillTimer(hwnd, REGISTER_TIMER_ID) };
        }
    }

    fn hover_event(
        &self,
        hwnd: HWND,
        rect: Rect,
        position: PhysicalPosition<f64>,
    ) -> Option<TrayIconEvent> {
        match self.last_position.get() {
            Some(last) if last == position => None,
            Some(_) => {
                self.last_position.set(Some(position));
                Some(TrayIconEvent::Move {
                    id: self.id.clone(),
                    rect,
                    position,
                })
            }
            None => {
                // A single mouse move must be followed by a leave check so
                // the next visit sends Enter again.
                if unsafe { SetTimer(hwnd, LEAVE_TIMER_ID, 15, None) } != 0 {
                    self.last_position.set(Some(position));
                }
                Some(TrayIconEvent::Enter {
                    id: self.id.clone(),
                    rect,
                    position,
                })
            }
        }
    }

    fn register_or_retry(&self, hwnd: HWND) -> crate::Result<()> {
        let mut nid = self.notification(hwnd, NIF_MESSAGE | NIF_STATE);
        nid.uCallbackMessage = WM_USER_TRAYICON;
        nid.dwState = if self.visible.get() { 0 } else { NIS_HIDDEN };
        nid.dwStateMask = NIS_HIDDEN;
        // Hold the icon handle alive without retaining a RefCell borrow when
        // the shell call can reenter this window's procedure.
        let icon = self.icon.borrow().clone();
        if let Some(icon) = &icon {
            nid.uFlags |= NIF_ICON;
            nid.hIcon = icon.inner.as_raw_handle();
        }
        if let Some(tooltip) = self.tooltip.borrow().as_deref() {
            nid.uFlags |= NIF_TIP;
            nid.szTip = encode_tooltip(tooltip);
        }
        let registered = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) } != 0;
        self.update_registration_retry(hwnd, registered)
    }

    fn update_registration_retry(&self, hwnd: HWND, registered: bool) -> crate::Result<()> {
        if registered {
            self.cancel_registration_retry(hwnd);
            return Ok(());
        }
        // Explorer may still be starting. Only poll for an absent icon;
        // success stops the timer, including after TaskbarCreated.
        let delay = self.next_retry_ms.get().max(INITIAL_REGISTER_RETRY_MS);
        if unsafe { SetTimer(hwnd, REGISTER_TIMER_ID, delay, None) } == 0 {
            let error = io::Error::last_os_error();
            self.cancel_registration_retry(hwnd);
            return Err(error.into());
        }
        self.next_retry_ms
            .set((delay * 2).min(MAX_REGISTER_RETRY_MS));
        Ok(())
    }
}

pub struct TrayIcon {
    hwnd: HWND,
    menu: Option<Box<dyn menu::ContextMenu>>,
    data: Rc<TrayUserData>,
}

impl TrayIcon {
    pub fn new(id: TrayIconId, attrs: TrayIconAttributes) -> crate::Result<Self> {
        let internal_id = COUNTER.next();
        let guid = attrs.guid.map(GUID::from_u128);

        let class_name = util::encode_wide("tray_icon_app");
        unsafe {
            let hinstance = util::get_instance_handle();

            let wnd_class = WNDCLASSW {
                lpfnWndProc: Some(tray_proc),
                lpszClassName: class_name.as_ptr(),
                hInstance: hinstance,
                ..std::mem::zeroed()
            };

            RegisterClassW(&wnd_class);

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED |
            // WS_EX_TOOLWINDOW prevents this window from ever showing up in the taskbar, which
            // we want to avoid. If you remove this style, this window won't show up in the
            // taskbar *initially*, but it can show up at some later point. This can sometimes
            // happen on its own after several hours have passed, although this has proven
            // difficult to reproduce. Alternatively, it can be manually triggered by killing
            // `explorer.exe` and then starting the process back up.
            // It is unclear why the bug is triggered by waiting for several hours.
            WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                ptr::null(),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                0,
                CW_USEDEFAULT,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                ptr::null(),
            );
            if hwnd.is_null() {
                return Err(io::Error::last_os_error().into());
            }

            let data = Rc::new(TrayUserData {
                id,
                internal_id,
                guid,
                hpopupmenu: Cell::new(attrs.menu.as_ref().map(|m| m.hpopupmenu() as _)),
                icon: RefCell::new(attrs.icon),
                tooltip: RefCell::new(attrs.tooltip),
                last_position: Cell::new(None),
                menu_on_left_click: Cell::new(attrs.menu_on_left_click),
                menu_on_right_click: Cell::new(attrs.menu_on_right_click),
                visible: Cell::new(true),
                next_retry_ms: Cell::new(0),
            });
            // Attach a reference only after CreateWindowExW succeeds. Both
            // the tray and its HWND use this state on their owning UI thread.
            let window_data = Rc::into_raw(data.clone());
            SetLastError(0);
            if util::set_window_long(hwnd, GWL_USERDATA, window_data as isize) == 0
                && GetLastError() != 0
            {
                let error = io::Error::last_os_error();
                drop(Rc::from_raw(window_data));
                DestroyWindow(hwnd);
                return Err(error.into());
            }

            // Allow "TaskbarCreated" through UIPI so elevated apps can re-register on explorer restart.
            ChangeWindowMessageFilterEx(hwnd, *S_U_TASKBAR_RESTART, MSGFLT_ALLOW, ptr::null_mut());

            if let Some(menu) = &attrs.menu {
                menu.attach_menu_subclass_for_hwnd(hwnd as _);
            }

            let tray = Self {
                hwnd,
                data,
                menu: attrs.menu,
            };
            tray.data.register_or_retry(hwnd)?;
            Ok(tray)
        }
    }

    pub fn set_icon(&mut self, icon: Option<Icon>) -> crate::Result<()> {
        let mut nid = self.data.notification(self.hwnd, NIF_ICON);
        if let Some(icon) = &icon {
            nid.hIcon = icon.inner.as_raw_handle();
        }
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } == 0 {
            return Err(io::Error::other("Windows rejected the tray icon update").into());
        }
        self.data.icon.replace(icon);
        Ok(())
    }

    pub fn set_menu(&mut self, menu: Option<Box<dyn menu::ContextMenu>>) {
        // Safety: self.hwnd is valid as long as as the TrayIcon is
        if let Some(menu) = &self.menu {
            unsafe { menu.detach_menu_subclass_from_hwnd(self.hwnd as _) };
        }
        if let Some(menu) = &menu {
            unsafe { menu.attach_menu_subclass_for_hwnd(self.hwnd as _) };
        }

        self.data
            .hpopupmenu
            .set(menu.as_ref().map(|m| m.hpopupmenu() as _));
        self.menu = menu;
    }

    pub fn set_tooltip<S: AsRef<str>>(&mut self, tooltip: Option<S>) -> crate::Result<()> {
        let mut nid = self.data.notification(self.hwnd, NIF_TIP);
        if let Some(tooltip) = &tooltip {
            nid.szTip = encode_tooltip(tooltip.as_ref());
        }
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } == 0 {
            return Err(io::Error::other("Windows rejected the tray tooltip update").into());
        }
        self.data
            .tooltip
            .replace(tooltip.map(|t| t.as_ref().to_owned()));
        Ok(())
    }

    pub fn set_show_menu_on_left_click(&mut self, enable: bool) {
        self.data.menu_on_left_click.set(enable);
    }

    pub fn set_show_menu_on_right_click(&mut self, enable: bool) {
        self.data.menu_on_right_click.set(enable);
    }

    pub fn show_menu(&self) {
        if let Some(menu) = &self.menu {
            unsafe {
                let mut cursor = POINT::default();
                if GetCursorPos(&mut cursor) != 0 {
                    show_tray_menu(self.hwnd, menu.hpopupmenu() as _, cursor.x, cursor.y);
                }
            }
        }
    }

    pub fn set_title<S: AsRef<str>>(&mut self, _title: Option<S>) {}

    pub fn set_visible(&mut self, visible: bool) -> crate::Result<()> {
        // Keep the desired state for registration after an Explorer restart.
        self.data.visible.set(visible);
        let mut nid = self.data.notification(self.hwnd, NIF_STATE);
        nid.dwState = if visible { 0 } else { NIS_HIDDEN };
        nid.dwStateMask = NIS_HIDDEN;
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } == 0 {
            return Err(io::Error::other("Windows rejected the tray visibility change").into());
        }
        Ok(())
    }

    pub fn rect(&self) -> Option<Rect> {
        get_tray_rect(self.data.internal_id, self.data.guid, self.hwnd).map(Into::into)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        if !self.data.is_attached(self.hwnd) {
            return;
        }
        unsafe {
            release_window_data(self.hwnd);
            Shell_NotifyIconW(NIM_DELETE, &self.data.notification(self.hwnd, 0));

            if let Some(menu) = &self.menu {
                menu.detach_menu_subclass_from_hwnd(self.hwnd as _);
            }

            // destroy the hidden window used by the tray
            DestroyWindow(self.hwnd);
        }
    }
}

/// The HWND slot owns one reference between attachment and destruction.
unsafe fn release_window_data(hwnd: HWND) {
    let data = util::set_window_long(hwnd, GWL_USERDATA, 0) as *const TrayUserData;
    if !data.is_null() {
        drop(Rc::from_raw(data));
    }
}

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        release_window_data(hwnd);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let data = util::get_window_long(hwnd, GWL_USERDATA) as *const TrayUserData;
    if data.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    // Keep the state alive if an event handler closes the tray within this
    // call. Event dispatch does not hold any of the RefCell borrows.
    Rc::increment_strong_count(data);
    let userdata = Rc::from_raw(data);
    match msg {
        _ if msg == *S_U_TASKBAR_RESTART => {
            Shell_NotifyIconW(NIM_DELETE, &userdata.notification(hwnd, 0));
            userdata.cancel_registration_retry(hwnd);
            if let Err(error) = userdata.register_or_retry(hwnd) {
                eprintln!("could not schedule restoring the system tray icon: {error}");
            }
            return 0;
        }
        WM_TIMER if wparam == REGISTER_TIMER_ID => {
            if userdata.next_retry_ms.get() != 0 {
                if let Err(error) = userdata.register_or_retry(hwnd) {
                    eprintln!("could not reschedule restoring the system tray icon: {error}");
                }
            }
            return 0;
        }

        WM_USER_TRAYICON
            if matches!(
                lparam as u32,
                WM_LBUTTONDOWN
                    | WM_RBUTTONDOWN
                    | WM_MBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_RBUTTONUP
                    | WM_MBUTTONUP
                    | WM_LBUTTONDBLCLK
                    | WM_RBUTTONDBLCLK
                    | WM_MBUTTONDBLCLK
                    | WM_MOUSEMOVE
            ) =>
        {
            let mut cursor = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut cursor as _) == 0 {
                return 0;
            }

            let position = PhysicalPosition::new(cursor.x as f64, cursor.y as f64);

            let rect = match get_tray_rect(userdata.internal_id, userdata.guid, hwnd) {
                Some(rect) => Rect::from(rect),
                None => return 0,
            };
            if lparam as u32 == WM_MOUSEMOVE {
                if let Some(event) = userdata.hover_event(hwnd, rect, position) {
                    TrayIconEvent::send(event);
                }
                return 0;
            }

            let id = userdata.id.clone();
            let event = match lparam as u32 {
                WM_LBUTTONDOWN => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Down,
                },
                WM_RBUTTONDOWN => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Down,
                },
                WM_MBUTTONDOWN => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Middle,
                    button_state: MouseButtonState::Down,
                },
                WM_LBUTTONUP => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                },
                WM_RBUTTONUP => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                },
                WM_MBUTTONUP => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Middle,
                    button_state: MouseButtonState::Up,
                },
                WM_LBUTTONDBLCLK => TrayIconEvent::DoubleClick {
                    id,
                    rect,
                    position,
                    button: MouseButton::Left,
                },
                WM_RBUTTONDBLCLK => TrayIconEvent::DoubleClick {
                    id,
                    rect,
                    position,
                    button: MouseButton::Right,
                },
                WM_MBUTTONDBLCLK => TrayIconEvent::DoubleClick {
                    id,
                    rect,
                    position,
                    button: MouseButton::Middle,
                },
                _ => unreachable!(),
            };

            TrayIconEvent::send(event);

            let show_menu = match lparam as u32 {
                WM_RBUTTONUP => userdata.menu_on_right_click.get(),
                WM_LBUTTONUP => userdata.menu_on_left_click.get(),
                _ => false,
            };
            if show_menu && userdata.is_attached(hwnd) {
                if let Some(menu) = userdata.hpopupmenu.get() {
                    show_tray_menu(hwnd, menu, cursor.x, cursor.y);
                }
            }
        }

        WM_TIMER if wparam == LEAVE_TIMER_ID => {
            let Some(position) = userdata.last_position.get() else {
                return 0;
            };

            let mut cursor = POINT { x: 0, y: 0 };
            let cursor_known = GetCursorPos(&mut cursor as _) != 0;
            let rect = get_tray_rect(userdata.internal_id, userdata.guid, hwnd);

            // A cursor or an icon that can't be found (a locked screen, the icon hidden in a
            // closed overflow area, explorer restarting) counts as left: the timer must not go
            // on firing until they can be found again.
            let inside = cursor_known
                && rect.is_some_and(|rect| {
                    let in_x = (rect.left..rect.right).contains(&cursor.x);
                    let in_y = (rect.top..rect.bottom).contains(&cursor.y);
                    in_x && in_y
                });

            if !inside {
                KillTimer(hwnd, LEAVE_TIMER_ID);
                userdata.last_position.set(None);

                TrayIconEvent::send(TrayIconEvent::Leave {
                    id: userdata.id.clone(),
                    rect: rect.unwrap_or_default().into(),
                    position,
                });
            }

            return 0;
        }

        _ => {}
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[inline]
unsafe fn show_tray_menu(hwnd: HWND, menu: HMENU, x: i32, y: i32) {
    // bring the hidden window to the foreground so the pop up menu
    // would automatically hide on click outside
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        // align bottom / right, maybe we could expose this later..
        TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        x,
        y,
        0,
        hwnd,
        std::ptr::null_mut(),
    );
    // The shell docs recommend posting a benign message after TrackPopupMenu
    // for notification area menus so the task switch is finalized correctly.
    PostMessageW(hwnd, WM_NULL, 0, 0);
}

fn encode_tooltip(tooltip: &str) -> [u16; 128] {
    let mut encoded = [0; 128];
    let mut offset = 0;
    for ch in tooltip.chars() {
        let end = offset + ch.len_utf16();
        if ch == '\0' || end >= encoded.len() {
            break;
        }
        ch.encode_utf16(&mut encoded[offset..end]);
        offset = end;
    }
    encoded
}

#[test]
fn tooltip_reserves_terminator_without_splitting_surrogates() {
    let ascii = encode_tooltip(&"a".repeat(128));
    assert_eq!(&ascii[..127], &[b'a' as u16; 127]);
    assert_eq!(ascii[127], 0);

    let fits = format!("{}😀", "a".repeat(125));
    let encoded = encode_tooltip(&fits);
    assert_eq!(String::from_utf16(&encoded[..127]).unwrap(), fits);
    assert_eq!(encoded[127], 0);

    let truncated = encode_tooltip(&format!("{}😀", "a".repeat(126)));
    assert_eq!(truncated[126..], [0, 0]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

    #[test]
    fn first_hover_arms_leave_tracking_and_extra_motion_is_ignored() {
        let tray = TrayIcon::new(TrayIconId::new_unique(), TrayIconAttributes::default()).unwrap();
        let rect = Rect::default();
        let position = PhysicalPosition::new(100.0, 100.0);
        assert!(matches!(
            tray.data.hover_event(tray.hwnd, rect, position),
            Some(TrayIconEvent::Enter { .. })
        ));
        assert!(tray.data.hover_event(tray.hwnd, rect, position).is_none());
        // KillTimer only succeeds if the first event really armed the timer.
        assert_ne!(unsafe { KillTimer(tray.hwnd, LEAVE_TIMER_ID) }, 0);
    }

    #[test]
    fn failed_registration_backs_off_and_success_stops_the_timer() {
        let tray = TrayIcon::new(TrayIconId::new_unique(), TrayIconAttributes::default()).unwrap();
        let (data, hwnd) = (&tray.data, tray.hwnd);
        data.cancel_registration_retry(hwnd);
        data.update_registration_retry(hwnd, false).unwrap();
        assert_eq!(data.next_retry_ms.get(), INITIAL_REGISTER_RETRY_MS * 2);
        assert_ne!(unsafe { KillTimer(hwnd, REGISTER_TIMER_ID) }, 0);
        for _ in 0..6 {
            data.update_registration_retry(hwnd, false).unwrap();
        }
        assert_eq!(data.next_retry_ms.get(), MAX_REGISTER_RETRY_MS);
        data.update_registration_retry(hwnd, true).unwrap();
        assert_eq!(data.next_retry_ms.get(), 0);
        assert_eq!(unsafe { KillTimer(hwnd, REGISTER_TIMER_ID) }, 0);
    }

    #[test]
    fn destroying_the_tray_window_releases_its_state() {
        for destroy_first in [false, true] {
            let tray =
                TrayIcon::new(TrayIconId::new_unique(), TrayIconAttributes::default()).unwrap();
            let hwnd = tray.hwnd;
            let data = Rc::downgrade(&tray.data);
            assert_eq!(Rc::strong_count(&tray.data), 2);
            if destroy_first {
                assert_ne!(unsafe { DestroyWindow(hwnd) }, 0);
                assert_eq!(Rc::strong_count(&tray.data), 1);
            }
            drop(tray);
            assert!(data.upgrade().is_none());
            assert_eq!(unsafe { IsWindow(hwnd) }, 0);
        }
    }
}

/// Stamp the optional stable identity onto a `NOTIFYICONDATAW`.
///
/// With `NIF_GUID` set, the shell identifies the icon by `guidItem` instead of
/// `(hWnd, uID)`. That identity is what Windows keys the user's "always show"
/// (promoted) setting on, so a fixed GUID lets the setting survive the binary
/// being replaced by an update.
#[inline]
fn apply_guid(nid: &mut NOTIFYICONDATAW, guid: Option<GUID>) {
    if let Some(guid) = guid {
        nid.uFlags |= NIF_GUID;
        nid.guidItem = guid;
    }
}

#[inline]
fn get_tray_rect(id: u32, guid: Option<GUID>, hwnd: HWND) -> Option<RECT> {
    let mut nid = NOTIFYICONIDENTIFIER {
        hWnd: hwnd,
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as _,
        uID: id,
        ..unsafe { std::mem::zeroed() }
    };
    if let Some(guid) = guid {
        nid.guidItem = guid;
    }

    let mut rect = RECT {
        left: 0,
        bottom: 0,
        right: 0,
        top: 0,
    };
    if unsafe { Shell_NotifyIconGetRect(&nid, &mut rect) } == S_OK {
        Some(rect)
    } else {
        None
    }
}

impl From<RECT> for Rect {
    fn from(rect: RECT) -> Self {
        Self {
            position: crate::dpi::PhysicalPosition::new(rect.left.into(), rect.top.into()),
            size: crate::dpi::PhysicalSize::new(
                rect.right.saturating_sub(rect.left) as u32,
                rect.bottom.saturating_sub(rect.top) as u32,
            ),
        }
    }
}
