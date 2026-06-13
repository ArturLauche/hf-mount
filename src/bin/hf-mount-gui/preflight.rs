//! Environment readiness checks: elevation, NFS client tools, portmapper
//! availability, mount-point validity. Shared by the Setup tab, the Mount
//! tab's blocker banner, and the `--check-setup` CLI command.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckLevel {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
pub struct CheckItem {
    pub level: CheckLevel,
    pub label: String,
    pub detail: String,
}

impl CheckItem {
    fn pass(label: &str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Pass,
            label: label.to_string(),
            detail: detail.into(),
        }
    }

    // Only the Windows checks produce warnings today.
    #[cfg(windows)]
    fn warn(label: &str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Warn,
            label: label.to_string(),
            detail: detail.into(),
        }
    }

    fn fail(label: &str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Fail,
            label: label.to_string(),
            detail: detail.into(),
        }
    }
}

pub fn check_level_label(level: CheckLevel) -> &'static str {
    match level {
        CheckLevel::Pass => "OK",
        CheckLevel::Warn => "WARN",
        CheckLevel::Fail => "FAIL",
    }
}

pub fn summarize_checks(checks: &[CheckItem]) -> String {
    if checks.iter().any(|check| check.level == CheckLevel::Fail) {
        "Setup checks found a blocking issue".to_string()
    } else if checks.iter().any(|check| check.level == CheckLevel::Warn) {
        "Setup checks passed with warnings".to_string()
    } else {
        "Setup checks passed".to_string()
    }
}

/// Shell command (or instruction) the user can run to fix a failing check.
pub fn blocker_command(check: &CheckItem) -> Option<&'static str> {
    #[cfg(windows)]
    {
        match check.label.as_str() {
            "Client for NFS" => Some(crate::platform::windows_enable_nfs_command()),
            "Administrator" => Some("Start-Process hf-mount-gui.exe -Verb RunAs"),
            "Portmapper" => Some("Close other NFS/portmap services or another hf-mount instance, then recheck."),
            "Mount point" => Some("Use an unused drive letter such as Z:, Y:, or X:."),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = check;
        None
    }
}

/// Run all platform checks. Spawns short-lived helper processes on Windows —
/// call from event handlers / startup, not on every frame.
pub fn run_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    #[cfg(windows)]
    {
        windows_preflight_checks(mount_point)
    }
    #[cfg(target_os = "macos")]
    {
        macos_preflight_checks(mount_point)
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        linux_preflight_checks(mount_point)
    }
}

#[cfg(windows)]
fn windows_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    use std::path::Path;

    let mut checks = Vec::new();
    let elevated = crate::platform::windows_is_elevated();
    checks.push(if elevated {
        CheckItem::pass("Administrator", "The GUI is elevated.")
    } else {
        CheckItem::fail(
            "Administrator",
            "Restart as Administrator so hf-mount can bind the local NFS portmapper.",
        )
    });

    let mount_exe = hf_mount::windows::system32_exe("mount.exe");
    let umount_exe = hf_mount::windows::system32_exe("umount.exe");
    checks.push(if mount_exe.exists() && umount_exe.exists() {
        CheckItem::pass("Client for NFS", "mount.exe and umount.exe are available.")
    } else {
        CheckItem::fail(
            "Client for NFS",
            "Enable Microsoft's Client for NFS optional feature and reboot if Windows asks.",
        )
    });

    checks.push(if elevated {
        windows_portmapper_check()
    } else {
        CheckItem::warn("Portmapper", "Port 111 is checked after elevation.")
    });

    let trimmed = mount_point.trim();
    checks.push(if trimmed.is_empty() {
        CheckItem::fail("Mount point", "Choose a drive letter like Z: or an empty NTFS directory.")
    } else if let Some(drive) = hf_mount::windows::drive_letter(trimmed) {
        let probe = format!("{drive}:\\");
        if Path::new(&probe).exists() {
            CheckItem::fail(
                "Mount point",
                format!("{drive}: already exists. Pick an unused drive letter such as Y: or X:."),
            )
        } else {
            CheckItem::pass("Mount point", format!("{drive}: is a free drive-letter target."))
        }
    } else {
        let path = Path::new(trimmed);
        if !path.is_absolute() {
            CheckItem::fail("Mount point", "Use a drive letter or an absolute directory path.")
        } else if path.exists() && !path.is_dir() {
            CheckItem::fail("Mount point", "The target exists but is not a directory.")
        } else if path.exists() {
            CheckItem::warn(
                "Mount point",
                "Directory target is absolute. A drive letter such as Z: is still the most reliable Windows target.",
            )
        } else {
            CheckItem::warn(
                "Mount point",
                "Directory target is absolute and will be created if the Windows NFS client accepts it. A drive letter is more reliable.",
            )
        }
    });
    checks
}

#[cfg(windows)]
fn windows_portmapper_check() -> CheckItem {
    use std::net::{TcpListener, UdpSocket};

    match (
        UdpSocket::bind(("127.0.0.1", 111)),
        TcpListener::bind(("127.0.0.1", 111)),
    ) {
        (Ok(udp), Ok(tcp)) => {
            drop(udp);
            drop(tcp);
            CheckItem::pass(
                "Portmapper",
                "TCP and UDP port 111 are available for the local NFS portmapper.",
            )
        }
        (udp_result, tcp_result) => CheckItem::fail(
            "Portmapper",
            format!(
                "Port 111 is not available: UDP={} TCP={}. Close other NFS/portmap services or another hf-mount instance.",
                bind_result_label(&udp_result),
                bind_result_label(&tcp_result),
            ),
        ),
    }
}

#[cfg(windows)]
fn bind_result_label<T>(result: &std::io::Result<T>) -> String {
    match result {
        Ok(_) => "ok".to_string(),
        Err(e) => e.to_string(),
    }
}

#[cfg(target_os = "macos")]
fn macos_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    use std::path::Path;

    let mount_cmd_exists = Path::new("/sbin/mount_nfs").exists();
    let mount_path_absolute = Path::new(mount_point.trim()).is_absolute();
    vec![
        if mount_cmd_exists {
            CheckItem::pass("mount_nfs", "/sbin/mount_nfs is available.")
        } else {
            CheckItem::fail("mount_nfs", "/sbin/mount_nfs was not found.")
        },
        if mount_path_absolute {
            CheckItem::pass("Mount point", "Mount point is an absolute path.")
        } else {
            CheckItem::fail("Mount point", "Use an absolute local directory path.")
        },
    ]
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn linux_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    use std::path::Path;

    let mount_path_absolute = Path::new(mount_point.trim()).is_absolute();
    vec![if mount_path_absolute {
        CheckItem::pass("Mount point", "Mount point is an absolute path.")
    } else {
        CheckItem::fail("Mount point", "Use an absolute local directory path.")
    }]
}
