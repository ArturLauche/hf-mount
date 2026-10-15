//! Starting the background supervisor.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

use crate::manager::config::home_dir;

/// Start `<exe> supervise` fully detached from the caller: no console (so
/// closing the terminal does not end it), its own process group (Ctrl+C in
/// the terminal does not reach it) and, when the caller's job allows it,
/// outside the caller's job object (so terminals or IDEs that kill their job
/// on exit do not take the mounts down).
///
/// Secrets in the caller's environment are not inherited: the supervisor
/// only uses credentials sent over the control pipe for a specific mount.
pub fn spawn_supervisor(exe: &Path) -> std::io::Result<u32> {
    stop_std_handle_inheritance();
    let home = home_dir();
    let _ = std::fs::create_dir_all(&home);
    let build = |flags: u32| {
        let mut cmd = Command::new(exe);
        cmd.arg("supervise")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_remove("HF_TOKEN")
            .env_remove("HUGGING_FACE_HUB_TOKEN")
            .current_dir(&home)
            .creation_flags(flags);
        cmd
    };
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    match build(base | CREATE_BREAKAWAY_FROM_JOB).spawn() {
        Ok(child) => Ok(child.id()),
        // The caller's job does not permit breakaway: start inside it.
        Err(e) if e.raw_os_error() == Some(5) => build(base).spawn().map(|c| c.id()),
        Err(e) => Err(e),
    }
}

/// `std::process::Command` always creates children with handle inheritance
/// enabled, so every inheritable handle of this process leaks into the
/// supervisor, whatever its own stdio is set to. The important one is the
/// pipe a calling shell reads our output from (`hf-mount start ... | ...`,
/// CI steps): the shell waits for every writer of that pipe to close, i.e.
/// for the long-lived supervisor to exit. No child of this process needs our
/// standard handles, so stop them from being inherited.
fn stop_std_handle_inheritance() {
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle has no preconditions; SetHandleInformation only
        // changes the inherit flag of a handle this process owns. Failures
        // (no handle, console pseudo-handles) are harmless and ignored.
        unsafe {
            let handle = GetStdHandle(id);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
