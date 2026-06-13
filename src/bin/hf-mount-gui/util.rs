//! Small shared helpers: text normalization, timestamps, atomic file writes.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

static BACKEND_INIT: Once = Once::new();

/// Initialize tracing and fd limits exactly once per process. Both the GUI
/// window and the `--background-worker` entry point go through this.
pub fn init_backend_once() {
    BACKEND_INIT.call_once(|| {
        hf_mount::setup::raise_fd_limit();
        hf_mount::setup::init_tracing(false);
    });
}

/// `Some(trimmed)` when the input has non-whitespace content.
pub fn optional_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn non_empty_or_default(text: &str, default: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn parse_path(text: &str, label: &str) -> Result<PathBuf, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required."));
    }
    Ok(PathBuf::from(trimmed))
}

pub fn current_env_hf_token() -> Option<String> {
    std::env::var("HF_TOKEN").ok().and_then(|token| optional_text(&token))
}

pub fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn current_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

pub fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Compact `1h 02m` / `5m 12s` / `42s` rendering for the status bar.
pub fn format_elapsed(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Write `bytes` to `path` atomically: temp sibling + rename. The temp file is
/// owner-private on Unix.
pub fn write_file_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    let temp_path = temp_sibling_path(path);
    write_private_file(&temp_path, bytes).map_err(|e| format!("Failed to write {}: {e}", temp_path.display()))?;

    // std::fs::rename replaces an existing destination atomically on Unix and
    // via MoveFileExW(REPLACE_EXISTING) on Windows. Don't pre-delete the
    // destination: a crash between delete and rename would lose the old file.
    std::fs::rename(&temp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        format!("Failed to replace {} with {}: {e}", path.display(), temp_path.display())
    })
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)
    }
}

fn temp_sibling_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "hf-mount".into());
    let unique = format!(".{file_name}.{}.{}.tmp", std::process::id(), current_unix_nanos());
    path.with_file_name(unique)
}

/// Per-user config directory for the GUI (profile, worker status, logs).
pub fn app_config_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "APPDATA is not set.".to_string())?;
        Ok(base.join("hf-mount"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set.".to_string())?;
        Ok(home.join("Library").join("Application Support").join("hf-mount"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
            return Ok(base.join("hf-mount"));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set.".to_string())?;
        Ok(home.join(".config").join("hf-mount"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_text_trims_and_filters() {
        assert_eq!(optional_text("  "), None);
        assert_eq!(optional_text(" x "), Some("x".to_string()));
    }

    #[test]
    fn format_elapsed_renders_each_magnitude() {
        assert_eq!(format_elapsed(42), "42s");
        assert_eq!(format_elapsed(312), "5m 12s");
        assert_eq!(format_elapsed(3720), "1h 02m");
    }

    #[test]
    fn write_file_replace_is_atomic_and_overwrites() {
        let dir = std::env::temp_dir().join(format!("hf-mount-gui-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        write_file_replace(&path, b"one").unwrap();
        write_file_replace(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
