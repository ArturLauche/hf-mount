//! Small Win32 helpers shared by the Windows modules.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// NUL-terminated UTF-16 copy of `s` for passing to `*W` APIs.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Read a NUL-terminated UTF-16 string of at most `max_units` code units.
/// Returns `None` for null pointers, invalid UTF-16 (unpaired surrogates) and
/// strings longer than `max_units`, which are rejected rather than truncated
/// (a truncated path would name a different file).
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide_ptr(ptr: *const u16, max_units: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: the caller guarantees NUL termination, and the loop stops at
    // the NUL, so no unit past it is read (at most index `max_units`).
    while unsafe { *ptr.add(len) } != 0 {
        if len == max_units {
            return None;
        }
        len += 1;
    }
    let units = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16(units).ok()
}

/// `std::io::Error` for the calling thread's last Win32 error.
pub fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// Owned Win32 handle closed on drop.
pub struct OwnedHandle(pub windows_sys::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            // SAFETY: we own the handle.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}
