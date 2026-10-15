//! Starting the background supervisor.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

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
