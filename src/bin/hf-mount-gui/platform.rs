//! OS integration: unmounting, opening folders, process liveness, elevation,
//! drive-letter enumeration. Everything that shells out lives here so the UI
//! code stays platform-free.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
use hf_mount::windows::{drive_letter, system32_exe};

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

pub fn platform_label() -> &'static str {
    #[cfg(windows)]
    {
        "Windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        "Linux"
    }
}

pub fn default_mount_point() -> String {
    #[cfg(windows)]
    {
        // Prefer a letter that is actually unassigned right now.
        free_drive_letters()
            .first()
            .map(|letter| format!("{letter}:"))
            .unwrap_or_else(|| "Z:".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir().join("hf-mount").to_string_lossy().into_owned()
    }
}

pub fn default_mount_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Z:"
    }
    #[cfg(not(windows))]
    {
        "/tmp/hf-mount"
    }
}

pub fn mount_point_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Use an unused drive letter. Directory targets are less reliable."
    }
    #[cfg(not(windows))]
    {
        "Use an absolute folder path."
    }
}

// ── Unmount ───────────────────────────────────────────────────────────

/// Run the platform unmount command for `mount_point`. Blocking — call from a
/// worker thread, never from the UI thread (a wedged NFS mount can stall the
/// command for tens of seconds).
pub fn unmount_path(mount_point: &Path) -> Result<(), String> {
    let mount_point = mount_point
        .to_str()
        .ok_or_else(|| "Mount point is not valid UTF-8".to_string())?;
    let target = unmount_target(mount_point);

    let output = unmount_command(&target)
        .output()
        .map_err(|e| format!("Failed to run unmount command: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "Unmount failed with {}: stdout={} stderr={}",
        output.status,
        stdout.trim(),
        stderr.trim()
    ))
}

#[cfg(windows)]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new(system32_exe("umount.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    command.args(["-f", mount_point]);
    command
}

#[cfg(target_os = "macos")]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new("/sbin/umount");
    command.arg(mount_point);
    command
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new("umount");
    command.arg(mount_point);
    command
}

fn unmount_target(mount_point: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(drive) = drive_letter(mount_point) {
            return format!("{drive}:");
        }
    }
    mount_point.to_string()
}

// ── Open in file manager ──────────────────────────────────────────────

pub fn open_mount_point(mount_point: Option<&Path>) -> Result<(), String> {
    let mount_point = mount_point.ok_or_else(|| "No active mount point is recorded.".to_string())?;
    let target = open_target(mount_point)?;

    open_command(&target)
        .spawn()
        .map_err(|e| format!("Failed to open mount point: {e}"))?;
    Ok(())
}

fn open_target(mount_point: &Path) -> Result<String, String> {
    let text = mount_point
        .to_str()
        .ok_or_else(|| "Mount point is not valid UTF-8".to_string())?;
    #[cfg(windows)]
    {
        if let Some(drive) = drive_letter(text) {
            return Ok(format!("{drive}:\\"));
        }
    }
    Ok(text.to_string())
}

#[cfg(windows)]
fn open_command(target: &str) -> Command {
    let mut command = Command::new("explorer.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    command.arg(target);
    command
}

#[cfg(target_os = "macos")]
fn open_command(target: &str) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.arg(target);
    command
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn open_command(target: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(target);
    command
}

// ── Process management ────────────────────────────────────────────────

/// Whether a process with `pid` is currently running. Blocking on Windows
/// (spawns `tasklist.exe`) — call from the worker poller thread only.
#[cfg(windows)]
pub fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    let filter = format!("PID eq {pid}");
    let mut command = Command::new(system32_exe("tasklist.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command.args(["/FI", &filter, "/FO", "CSV", "/NH"]).output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains(&format!(",\"{pid}\","))
}

#[cfg(unix)]
pub fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    // SAFETY: kill with signal 0 only probes for existence/permission.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
pub fn process_is_running(_pid: u32) -> bool {
    false
}

/// Terminate a process by id. Used to stop a background worker that has not
/// mounted yet (unmounting cannot reach it). SIGTERM on Unix lets a mounted
/// worker unmount gracefully; on Windows `taskkill /T /F` also takes down any
/// helper children.
#[cfg(windows)]
pub fn terminate_process(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("invalid process id".to_string());
    }
    let mut command = Command::new(system32_exe("taskkill.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .map_err(|e| format!("Failed to run taskkill: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "taskkill failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(unix)]
pub fn terminate_process(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("invalid process id".to_string());
    }
    // SAFETY: kill with SIGTERM has no preconditions beyond a valid pid value.
    if unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "Failed to signal process {pid}: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(any(windows, unix)))]
pub fn terminate_process(_pid: u32) -> Result<(), String> {
    Err("process termination is not supported on this platform".to_string())
}

/// Whether the path looks like a live mount target. Blocking on a wedged NFS
/// mount — poller thread only.
pub fn mount_point_appears_active(mount_point: &Path) -> bool {
    #[cfg(windows)]
    if let Some(text) = mount_point.to_str()
        && let Some(drive) = drive_letter(text)
    {
        return Path::new(&format!("{drive}:\\")).exists();
    }

    mount_point.exists()
}

/// Detach a child process from the GUI so it survives window close.
pub fn detach_command(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(any(windows, unix)))]
    let _ = command;
}

// ── Windows elevation & setup actions ─────────────────────────────────

#[cfg(windows)]
pub fn windows_is_elevated() -> bool {
    let mut command = Command::new(system32_exe("fltmc.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.status().map(|status| status.success()).unwrap_or(false)
}

#[cfg(windows)]
fn powershell_exe() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

#[cfg(windows)]
pub fn windows_enable_nfs_command() -> &'static str {
    "Enable-WindowsOptionalFeature -Online -FeatureName ServicesForNFS-ClientOnly,ClientForNFS-Infrastructure -All"
}

/// Launch an elevated PowerShell that enables the Client for NFS feature.
/// The user still has to approve the UAC prompt.
#[cfg(windows)]
pub fn enable_windows_nfs_client() -> Result<(), String> {
    let powershell = powershell_exe();
    let elevated_args = format!(
        "-NoProfile -ExecutionPolicy Bypass -Command \"{}\"",
        windows_enable_nfs_command()
    );

    let status = Command::new(&powershell)
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Start-Process -FilePath $args[0] -Verb RunAs -ArgumentList $args[1]",
        ])
        .arg(&powershell)
        .arg(elevated_args)
        .status()
        .map_err(|e| format!("Failed to launch the UAC prompt: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell exited with {status}"))
    }
}

/// Relaunch the GUI elevated (UAC prompt). The current instance keeps running;
/// the user is expected to continue in the elevated window.
#[cfg(windows)]
pub fn restart_as_administrator() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not locate current executable: {e}"))?;

    let status = Command::new(powershell_exe())
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Start-Process -FilePath $args[0] -Verb RunAs",
        ])
        .arg(exe)
        .status()
        .map_err(|e| format!("Failed to launch the UAC prompt: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell exited with {status}"))
    }
}

// ── Drive letters (Windows) ───────────────────────────────────────────

/// Currently unassigned drive letters, best first (`Z:` downwards). Uses a
/// single `GetLogicalDrives` syscall — no per-letter filesystem probing, so it
/// never blocks on wedged network drives.
#[cfg(windows)]
pub fn free_drive_letters() -> Vec<char> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    // SAFETY: GetLogicalDrives takes no arguments and only returns a bitmask.
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        // Failure: report nothing rather than guessing wrong.
        return Vec::new();
    }
    hf_mount::windows::free_drive_letters(mask)
}
