// Copyright 2022-2022 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

// taken from https://github.com/rust-windowing/winit/blob/92fdf5ba85f920262a61cee4590f4a11ad5738d1/src/platform_impl/windows/icon.rs

use std::{fmt, io, path::Path, sync::Arc};

use windows_sys::{
    core::PCWSTR,
    Win32::UI::WindowsAndMessaging::{
        CreateIcon, DestroyIcon, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE,
    },
};

use crate::icon::*;

use super::util;

impl RgbaIcon {
    fn into_windows_icon(self) -> Result<WinIcon, BadIcon> {
        if self.width == 0
            || self.height == 0
            || self.width > i32::MAX as u32
            || self.height > i32::MAX as u32
        {
            return Err(BadIcon::OsError(io::Error::new(
                io::ErrorKind::InvalidInput,
                "icon dimensions must be positive 32-bit integers",
            )));
        }
        let mut rgba = self.rgba;
        let and_mask = prepare_pixels(&mut rgba, self.width as usize);
        let handle = unsafe {
            CreateIcon(
                std::ptr::null_mut(),
                self.width as i32,
                self.height as i32,
                1,
                (PIXEL_SIZE * 8) as u8,
                and_mask.as_ptr(),
                rgba.as_ptr(),
            )
        };
        if !handle.is_null() {
            Ok(WinIcon::from_handle(handle))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }
}

fn prepare_pixels(rgba: &mut [u8], width: usize) -> Vec<u8> {
    // CreateIcon takes a monochrome AND mask with word-aligned scanlines.
    let stride = width.div_ceil(16) * 2;
    let row_bytes = width * PIXEL_SIZE;
    let mut and_mask = vec![0; (rgba.len() / row_bytes) * stride];
    for (row, mask) in rgba
        .chunks_exact_mut(row_bytes)
        .zip(and_mask.chunks_exact_mut(stride))
    {
        let (pixels, _) = row.as_chunks_mut::<PIXEL_SIZE>();
        for (x, pixel) in pixels.iter_mut().enumerate() {
            if pixel[3] == 0 {
                mask[x / 8] |= 0x80 >> (x % 8);
            }
            pixel.swap(0, 2); // RGBA to BGRA
        }
    }
    and_mask
}

#[test]
fn monochrome_mask_packs_each_row_and_preserves_color_alpha() {
    assert_eq!(prepare_pixels(&mut [1, 2, 3, 0], 1), [0x80, 0]);
    let mut rgba = [1, 2, 3, 255].repeat(34);
    rgba[3] = 0;
    rgba[16 * PIXEL_SIZE + 3] = 0;
    rgba[18 * PIXEL_SIZE + 3] = 128;
    rgba[24 * PIXEL_SIZE + 3] = 0;
    assert_eq!(
        prepare_pixels(&mut rgba, 17),
        [0x80, 0, 0x80, 0, 0x01, 0, 0, 0]
    );
    assert_eq!(&rgba[18 * PIXEL_SIZE..19 * PIXEL_SIZE], &[3, 2, 1, 128]);
}

#[derive(Debug)]
struct RaiiIcon {
    handle: HICON,
}

#[derive(Clone)]
pub(crate) struct WinIcon {
    inner: Arc<RaiiIcon>,
}

unsafe impl Send for WinIcon {}

impl WinIcon {
    pub fn as_raw_handle(&self) -> HICON {
        self.inner.handle
    }

    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
        let rgba_icon = RgbaIcon::from_rgba(rgba, width, height)?;
        rgba_icon.into_windows_icon()
    }

    pub(crate) fn from_handle(handle: HICON) -> Self {
        Self {
            #[allow(clippy::arc_with_non_send_sync)]
            inner: Arc::new(RaiiIcon { handle }),
        }
    }

    pub(crate) fn from_path<P: AsRef<Path>>(
        path: P,
        size: Option<(u32, u32)>,
    ) -> Result<Self, BadIcon> {
        // width / height of 0 along with LR_DEFAULTSIZE tells windows to load the default icon size
        let (width, height) = size.unwrap_or((0, 0));

        let wide_path = util::encode_wide(path.as_ref());

        let handle = unsafe {
            LoadImageW(
                std::ptr::null_mut(),
                wide_path.as_ptr(),
                IMAGE_ICON,
                width as i32,
                height as i32,
                LR_DEFAULTSIZE | LR_LOADFROMFILE,
            )
        };
        if !handle.is_null() {
            Ok(WinIcon::from_handle(handle as HICON))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }

    fn from_resource_inner_name(name: PCWSTR, size: Option<(u32, u32)>) -> Result<Self, BadIcon> {
        // width / height of 0 along with LR_DEFAULTSIZE tells windows to load the default icon size
        let (width, height) = size.unwrap_or((0, 0));
        let handle = unsafe {
            LoadImageW(
                util::get_instance_handle(),
                name,
                IMAGE_ICON,
                width as i32,
                height as i32,
                LR_DEFAULTSIZE,
            )
        };
        if !handle.is_null() {
            Ok(WinIcon::from_handle(handle as HICON))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }

    pub(crate) fn from_resource(
        resource_id: u16,
        size: Option<(u32, u32)>,
    ) -> Result<Self, BadIcon> {
        Self::from_resource_inner_name(resource_id as PCWSTR, size)
    }

    pub(crate) fn from_resource_name(
        resource_name: &str,
        size: Option<(u32, u32)>,
    ) -> Result<Self, BadIcon> {
        let wide_name = util::encode_wide(resource_name);
        Self::from_resource_inner_name(wide_name.as_ptr(), size)
    }
}

impl Drop for RaiiIcon {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.handle) };
    }
}

impl fmt::Debug for WinIcon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        (*self.inner).fmt(formatter)
    }
}
