//! Start-at-login registration: a Scheduled Task on Windows, a LaunchAgent on
//! macOS, an XDG autostart entry on Linux desktops. All variants launch the
//! GUI executable with `--background-worker` using the saved profile.

use crate::worker::BACKGROUND_WORKER_ARG;

pub const AUTOSTART_NAME: &str = "hf-mount autostart";
#[cfg(target_os = "macos")]
const AUTOSTART_LABEL: &str = "co.huggingface.hf-mount-gui-autostart";

pub fn set_autostart_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        install_autostart()
    } else {
        remove_autostart()
    }
}

#[cfg(windows)]
pub fn autostart_is_enabled() -> bool {
    use std::process::Stdio;

    windows_schtasks()
        .args(["/Query", "/TN", AUTOSTART_NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(windows))]
pub fn autostart_is_enabled() -> bool {
    autostart_path().is_ok_and(|path| path.exists())
}

#[cfg(windows)]
fn install_autostart() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Could not locate current executable: {e}"))?
        .to_string_lossy()
        .replace('"', "\\\"");
    let task_run = format!("\"{exe}\" {BACKGROUND_WORKER_ARG}");
    let output = windows_schtasks()
        .args([
            "/Create",
            "/TN",
            AUTOSTART_NAME,
            "/TR",
            &task_run,
            "/SC",
            "ONLOGON",
            "/RL",
            "HIGHEST",
            "/F",
        ])
        .output()
        .map_err(|e| format!("Failed to create scheduled task: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_output_error("schtasks /Create", output))
    }
}

#[cfg(windows)]
fn remove_autostart() -> Result<(), String> {
    let output = windows_schtasks()
        .args(["/Delete", "/TN", AUTOSTART_NAME, "/F"])
        .output()
        .map_err(|e| format!("Failed to remove scheduled task: {e}"))?;
    if output.status.success() || !autostart_is_enabled() {
        Ok(())
    } else {
        Err(command_output_error("schtasks /Delete", output))
    }
}

#[cfg(windows)]
fn windows_schtasks() -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new(hf_mount::windows::system32_exe("schtasks.exe"));
    command.creation_flags(crate::platform::CREATE_NO_WINDOW);
    command
}

#[cfg(windows)]
fn command_output_error(label: &str, output: std::process::Output) -> String {
    format!(
        "{label} failed with {}: stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

#[cfg(not(windows))]
fn install_autostart() -> Result<(), String> {
    let path = autostart_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    let content = autostart_file_contents()?;
    std::fs::write(&path, content).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

#[cfg(not(windows))]
fn remove_autostart() -> Result<(), String> {
    let path = autostart_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn autostart_path() -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "HOME is not set.".to_string())?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{AUTOSTART_LABEL}.plist")))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn autostart_path() -> Result<std::path::PathBuf, String> {
    let base = if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from) {
        config_home
    } else {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "HOME is not set.".to_string())?;
        home.join(".config")
    };
    Ok(base.join("autostart").join("hf-mount.desktop"))
}

#[cfg(target_os = "macos")]
fn autostart_file_contents() -> Result<String, String> {
    let exe = xml_escape(
        &std::env::current_exe()
            .map_err(|e| format!("Could not locate current executable: {e}"))?
            .to_string_lossy(),
    );
    let name = xml_escape(AUTOSTART_NAME);
    let log_path = xml_escape(&crate::worker::worker_log_path()?.to_string_lossy());
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
             <key>Label</key>\n\
             <string>{AUTOSTART_LABEL}</string>\n\
             <key>ServiceDescription</key>\n\
             <string>{name}</string>\n\
             <key>ProgramArguments</key>\n\
             <array>\n\
                 <string>{exe}</string>\n\
                 <string>{BACKGROUND_WORKER_ARG}</string>\n\
             </array>\n\
             <key>RunAtLoad</key>\n\
             <true/>\n\
             <key>StandardOutPath</key>\n\
             <string>{log_path}</string>\n\
             <key>StandardErrorPath</key>\n\
             <string>{log_path}</string>\n\
         </dict>\n\
         </plist>\n"
    ))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn autostart_file_contents() -> Result<String, String> {
    let exe = desktop_exec_quote(
        &std::env::current_exe()
            .map_err(|e| format!("Could not locate current executable: {e}"))?
            .to_string_lossy(),
    );
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={AUTOSTART_NAME}\n\
         Exec={exe} {BACKGROUND_WORKER_ARG}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

#[cfg(target_os = "macos")]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn desktop_exec_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
