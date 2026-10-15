//! Console helpers.

use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};

/// Disables console echo (for secret input) until dropped.
pub struct EchoOff {
    previous: Option<CONSOLE_MODE>,
}

impl EchoOff {
    pub fn new() -> Self {
        // SAFETY: plain console API calls on the process's stdin handle.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode: CONSOLE_MODE = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return Self { previous: None };
            }
            SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT);
            Self { previous: Some(mode) }
        }
    }
}

impl Default for EchoOff {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some(mode) = self.previous {
            // SAFETY: restore the mode we read in `new`.
            unsafe { SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode) };
        }
    }
}
