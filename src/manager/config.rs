//! Per-user files: settings, saved mount profiles, logs, state and the
//! default cache location. Nothing here ever contains a secret.
//!
//! Layout (Windows): `%LOCALAPPDATA%\hf-mount\`
//! * `settings.json`, `profiles.json`
//! * `logs\` — redacted supervisor/app logs
//! * `state\` — supervisor session journals
//! * `cache\` — default chunk cache root (one subdirectory per source)
//! * `recovered\` — copies of files whose upload did not finish at unmount
//!
//! `HF_MOUNT_HOME` overrides the root (tests, portable use).

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::spec::MountSpec;

/// Root directory for hf-mount's per-user files.
pub fn home_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HF_MOUNT_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(local).join("hf-mount");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(data).join("hf-mount");
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".hf-mount"))
        .unwrap_or_else(|| std::env::temp_dir().join("hf-mount"))
}

pub fn logs_dir() -> PathBuf {
    home_dir().join("logs")
}

pub fn state_dir() -> PathBuf {
    home_dir().join("state")
}

pub fn recovered_dir() -> PathBuf {
    home_dir().join("recovered")
}

pub fn default_cache_root() -> PathBuf {
    home_dir().join("cache")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// User preferences shared by the CLI and the desktop app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Cache root; `None` = `default_cache_root()`.
    pub cache_dir: Option<PathBuf>,
    /// Chunk cache budget per source, bytes (upstream default 10 GB).
    pub cache_size_bytes: u64,
    /// How long `stop`/app quit waits for pending uploads, seconds.
    pub upload_wait_secs: u64,
    /// Metadata TTL in milliseconds (how long Windows may cache file info).
    pub metadata_ttl_ms: u64,
    /// Launch the desktop app (in the tray) when the user signs in.
    pub launch_at_login: bool,
    /// Re-mount profiles marked `auto_mount` when the app starts.
    pub reconnect_on_start: bool,
    /// Closing the window keeps the app in the tray.
    pub close_to_tray: bool,
    /// Quitting the app also unmounts all drives (otherwise they keep running
    /// in the background supervisor).
    pub unmount_on_quit: bool,
    /// Hub endpoint override (self-hosted / staging).
    pub hub_endpoint: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            cache_dir: None,
            cache_size_bytes: 10_000_000_000,
            upload_wait_secs: 300,
            metadata_ttl_ms: 10_000,
            launch_at_login: false,
            reconnect_on_start: true,
            close_to_tray: true,
            unmount_on_quit: false,
            hub_endpoint: None,
        }
    }
}

impl Settings {
    pub fn cache_root(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(default_cache_root)
    }

    /// Reject values that would make mounts misbehave.
    pub fn validate(&self) -> Result<(), String> {
        if self.cache_size_bytes < 100_000_000 {
            return Err("cache size must be at least 100 MB".into());
        }
        if self.upload_wait_secs > 24 * 3600 {
            return Err("upload wait must be at most 24 hours".into());
        }
        if self.metadata_ttl_ms > 3_600_000 {
            return Err("metadata TTL must be at most one hour".into());
        }
        if let Some(endpoint) = &self.hub_endpoint
            && !endpoint.starts_with("https://")
        {
            return Err("hub endpoint must be an https:// URL".into());
        }
        if let Some(dir) = &self.cache_dir
            && !dir.is_absolute()
        {
            return Err("cache directory must be an absolute path".into());
        }
        Ok(())
    }
}

/// A saved mount (the app's "drives" list). Holds no credentials: when
/// `use_stored_token` is set, the token is read from the OS credential store
/// at mount time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub spec: MountSpec,
    #[serde(default)]
    pub auto_mount: bool,
    #[serde(default = "yes")]
    pub use_stored_token: bool,
    /// RFC 3339.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_mounted_at: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<Profile>,
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> std::io::Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

/// Write via a temporary file and rename, so a crash never leaves a
/// truncated settings/profiles file.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn settings_path() -> PathBuf {
    home_dir().join("settings.json")
}

pub fn profiles_path() -> PathBuf {
    home_dir().join("profiles.json")
}

pub fn load_settings() -> std::io::Result<Settings> {
    read_json(&settings_path())
}

pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    settings
        .validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    write_json_atomic(&settings_path(), settings)
}

pub fn load_profiles() -> std::io::Result<Vec<Profile>> {
    Ok(read_json::<ProfilesFile>(&profiles_path())?.profiles)
}

pub fn save_profiles(profiles: &[Profile]) -> std::io::Result<()> {
    for p in profiles {
        p.spec
            .validated()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("profile {}: {e}", p.name)))?;
    }
    write_json_atomic(
        &profiles_path(),
        &ProfilesFile {
            profiles: profiles.to_vec(),
        },
    )
}

/// Insert or replace (by id) a profile.
pub fn upsert_profile(profile: Profile) -> std::io::Result<Vec<Profile>> {
    let mut profiles = load_profiles()?;
    match profiles.iter_mut().find(|p| p.id == profile.id) {
        Some(existing) => *existing = profile,
        None => profiles.push(profile),
    }
    save_profiles(&profiles)?;
    Ok(profiles)
}

pub fn delete_profile(id: &str) -> std::io::Result<Vec<Profile>> {
    let mut profiles = load_profiles()?;
    profiles.retain(|p| p.id != id);
    save_profiles(&profiles)?;
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `HF_MOUNT_HOME` is process-global; serialise tests that set it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: serialised by ENV_LOCK; no other thread reads this var concurrently.
        unsafe { std::env::set_var("HF_MOUNT_HOME", dir.path()) };
        let out = f(dir.path());
        unsafe { std::env::remove_var("HF_MOUNT_HOME") };
        out
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        with_home(|home| {
            assert_eq!(load_settings().unwrap(), Settings::default());
            let s = Settings {
                theme: Theme::Dark,
                upload_wait_secs: 60,
                ..Default::default()
            };
            save_settings(&s).unwrap();
            assert_eq!(load_settings().unwrap(), s);
            assert!(home.join("settings.json").exists());
            // Unknown/missing fields fall back to defaults.
            std::fs::write(home.join("settings.json"), r#"{"theme":"light","future_field":1}"#).unwrap();
            let loaded = load_settings().unwrap();
            assert_eq!(loaded.theme, Theme::Light);
            assert_eq!(loaded.cache_size_bytes, Settings::default().cache_size_bytes);
        });
    }

    #[test]
    fn invalid_settings_are_refused() {
        with_home(|_| {
            let s = Settings {
                cache_size_bytes: 10,
                ..Default::default()
            };
            assert!(save_settings(&s).is_err());
            let s = Settings {
                hub_endpoint: Some("http://insecure".into()),
                ..Default::default()
            };
            assert!(save_settings(&s).is_err());
        });
    }

    #[test]
    fn profiles_crud_never_store_secrets() {
        with_home(|home| {
            let spec = MountSpec::parse("bucket", "me/data", "X:").unwrap();
            let p = Profile {
                id: "p1".into(),
                name: "Data".into(),
                spec,
                auto_mount: true,
                use_stored_token: true,
                created_at: "2026-01-01T00:00:00Z".into(),
                last_mounted_at: None,
            };
            upsert_profile(p.clone()).unwrap();
            let mut renamed = p.clone();
            renamed.name = "Data (renamed)".into();
            let all = upsert_profile(renamed).unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(load_profiles().unwrap()[0].name, "Data (renamed)");
            let raw = std::fs::read_to_string(home.join("profiles.json")).unwrap();
            assert!(!raw.contains("hf_"), "profiles must not contain tokens");
            assert!(delete_profile("p1").unwrap().is_empty());
        });
    }

    #[test]
    fn corrupt_files_are_reported_not_silently_reset() {
        with_home(|home| {
            std::fs::write(home.join("profiles.json"), "{not json").unwrap();
            assert_eq!(load_profiles().unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        });
    }
}
