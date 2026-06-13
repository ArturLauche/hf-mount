//! Saved mount profile: what the user last configured, persisted as JSON in
//! the per-user config directory. Inline HF tokens are deliberately never
//! part of the profile — background and autostart mounts read `HF_TOKEN`
//! from the environment or a token file instead.

use std::path::PathBuf;

use hf_mount::setup::{CacheMode, MountOptions, Source};
use serde::{Deserialize, Serialize};

use crate::util::{
    app_config_dir, current_env_hf_token, non_empty_or_default, optional_text, parse_path, write_file_replace,
};

pub const MAX_RECENT_SOURCES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiSource {
    Repo,
    Bucket,
}

/// A previously mounted source, offered for one-click refill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSource {
    pub source: GuiSource,
    pub source_id: String,
    #[serde(default)]
    pub revision: String,
}

impl RecentSource {
    pub fn label(&self) -> String {
        match self.source {
            GuiSource::Repo if !self.revision.is_empty() && self.revision != "main" => {
                format!("repo {} @ {}", self.source_id, self.revision)
            }
            GuiSource::Repo => format!("repo {}", self.source_id),
            GuiSource::Bucket => format!("bucket {}", self.source_id),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MountProfile {
    pub source: GuiSource,
    pub source_id: String,
    pub revision: String,
    pub mount_point: String,
    #[serde(default)]
    pub token_file: String,
    pub hub_endpoint: String,
    pub cache_dir: String,
    pub read_only: bool,
    pub run_in_background: bool,
    #[serde(default)]
    pub nfs_allow_unsafe_loopback: bool,
    #[serde(default)]
    pub recent_sources: Vec<RecentSource>,
}

impl MountProfile {
    /// Record `entry` as the most recent source, deduplicated and capped.
    pub fn remember_recent(&mut self, entry: RecentSource) {
        self.recent_sources.retain(|existing| *existing != entry);
        self.recent_sources.insert(0, entry);
        self.recent_sources.truncate(MAX_RECENT_SOURCES);
    }
}

pub fn profile_path() -> Result<PathBuf, String> {
    Ok(app_config_dir()?.join("mount-profile.json"))
}

pub fn load_mount_profile() -> Result<Option<MountProfile>, String> {
    let path = profile_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))
}

pub fn save_mount_profile(profile: &MountProfile) -> Result<(), String> {
    let path = profile_path()?;
    let json = serde_json::to_vec_pretty(profile).map_err(|e| format!("Failed to serialize settings: {e}"))?;
    write_file_replace(&path, &json)
}

/// Validate the source id as typed in the form. Returns a human-readable
/// problem, or `None` when it looks plausible.
pub fn source_id_problem(source: GuiSource, source_id: &str) -> Option<&'static str> {
    let trimmed = source_id.trim();
    if trimmed.is_empty() {
        return Some(match source {
            GuiSource::Repo => "Repo ID is required, e.g. openai-community/gpt2.",
            GuiSource::Bucket => "Bucket ID is required, e.g. namespace/bucket.",
        });
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Some("IDs cannot contain spaces.");
    }
    if trimmed.starts_with('/') || trimmed.ends_with('/') {
        return Some("Remove the leading/trailing slash.");
    }
    if source == GuiSource::Bucket {
        // Buckets are namespace/bucket, optionally with a subfolder
        // (namespace/bucket/path/to/dir) — require at least two non-empty
        // segments and reject empty segments like `a//b`.
        let mut segments = trimmed.split('/');
        let namespace = segments.next().filter(|seg| !seg.is_empty());
        let bucket = segments.next().filter(|seg| !seg.is_empty());
        if namespace.is_none() || bucket.is_none() || segments.any(str::is_empty) {
            return Some("Buckets are namespace/bucket, e.g. myuser/my-bucket.");
        }
    }
    None
}

pub fn profile_mount_source(profile: &MountProfile) -> Result<Source, String> {
    if let Some(problem) = source_id_problem(profile.source, &profile.source_id) {
        return Err(problem.to_string());
    }
    let source_id = profile.source_id.trim();
    let mount_point = parse_path(&profile.mount_point, "Mount point")?;
    Ok(match profile.source {
        GuiSource::Repo => Source::Repo {
            repo_id: source_id.to_string(),
            mount_point,
            revision: non_empty_or_default(&profile.revision, "main"),
        },
        GuiSource::Bucket => Source::Bucket {
            bucket_id: source_id.to_string(),
            mount_point,
        },
    })
}

/// Mount options for the GUI's NFS backend. The token comes from `HF_TOKEN`
/// or the configured token file; an inline token (foreground mounts only) is
/// layered on by the caller.
pub fn profile_mount_options(profile: &MountProfile) -> Result<MountOptions, String> {
    Ok(MountOptions {
        hf_token: current_env_hf_token(),
        token_file: optional_text(&profile.token_file).map(PathBuf::from),
        hub_endpoint: non_empty_or_default(&profile.hub_endpoint, "https://huggingface.co"),
        cache_dir: parse_path(&profile.cache_dir, "Cache directory")?,
        uid: None,
        gid: None,
        read_only: profile.source == GuiSource::Repo || profile.read_only,
        advanced_writes: false,
        poll_interval_secs: 30,
        poll_listing_concurrency: 4,
        cache_size: 10_000_000_000,
        max_staging_size: 0,
        no_disk_cache: false,
        cache_mode: CacheMode::Chunk,
        direct_io: false,
        metadata_ttl_ms: 10_000,
        metadata_ttl_minimal: false,
        max_threads: 16,
        flush_debounce_ms: 2_000,
        flush_max_batch_window_ms: 30_000,
        no_filter_os_files: false,
        fuse_owner_only: false,
        fuse_allow_other: false,
        nfs_allow_unsafe_loopback: profile.nfs_allow_unsafe_loopback,
        inode_soft_limit: 0,
        lru_sweep_interval_ms: 5_000,
        overlay: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> MountProfile {
        MountProfile {
            source: GuiSource::Repo,
            source_id: "openai-community/gpt2".to_string(),
            revision: "main".to_string(),
            mount_point: "/tmp/hf-mount".to_string(),
            token_file: "/tmp/hf-token".to_string(),
            hub_endpoint: "https://huggingface.co".to_string(),
            cache_dir: "/tmp/hf-cache".to_string(),
            read_only: true,
            run_in_background: true,
            nfs_allow_unsafe_loopback: false,
            recent_sources: Vec::new(),
        }
    }

    #[test]
    fn mount_profile_serialization_excludes_inline_token() {
        let json = serde_json::to_string(&sample_profile()).unwrap();
        assert!(!json.contains("hf_token"));
        assert!(json.contains("token_file"));
    }

    #[test]
    fn old_profile_token_field_is_ignored_on_load() {
        let json = r#"{
            "source":"Repo",
            "source_id":"openai-community/gpt2",
            "revision":"main",
            "mount_point":"/tmp/hf-mount",
            "hf_token":"hf_secret",
            "hub_endpoint":"https://huggingface.co",
            "cache_dir":"/tmp/hf-cache",
            "read_only":true,
            "run_in_background":false
        }"#;
        let profile: MountProfile = serde_json::from_str(json).unwrap();
        let rewritten = serde_json::to_string(&profile).unwrap();
        assert!(!rewritten.contains("hf_secret"));
        assert!(!rewritten.contains("hf_token"));
    }

    #[test]
    fn recent_sources_dedupe_and_cap() {
        let mut profile = sample_profile();
        for i in 0..8 {
            profile.remember_recent(RecentSource {
                source: GuiSource::Repo,
                source_id: format!("user/model-{i}"),
                revision: "main".to_string(),
            });
        }
        assert_eq!(profile.recent_sources.len(), MAX_RECENT_SOURCES);
        assert_eq!(profile.recent_sources[0].source_id, "user/model-7");

        // Re-mounting an existing entry moves it to the front without duplicating.
        profile.remember_recent(RecentSource {
            source: GuiSource::Repo,
            source_id: "user/model-5".to_string(),
            revision: "main".to_string(),
        });
        assert_eq!(profile.recent_sources.len(), MAX_RECENT_SOURCES);
        assert_eq!(profile.recent_sources[0].source_id, "user/model-5");
    }

    #[test]
    fn source_id_validation_catches_common_mistakes() {
        assert!(source_id_problem(GuiSource::Repo, "").is_some());
        assert!(source_id_problem(GuiSource::Repo, "has space/model").is_some());
        assert!(source_id_problem(GuiSource::Repo, "/leading").is_some());
        assert!(source_id_problem(GuiSource::Bucket, "no-namespace").is_some());
        // Empty interior segments are rejected, but subfolder paths are valid:
        // Source::Bucket supports namespace/bucket/path/to/dir.
        assert!(source_id_problem(GuiSource::Bucket, "a//b").is_some());
        assert!(source_id_problem(GuiSource::Bucket, "namespace/bucket/checkpoints").is_none());
        assert!(source_id_problem(GuiSource::Repo, "gpt2").is_none());
        assert!(source_id_problem(GuiSource::Repo, "openai-community/gpt2").is_none());
        assert!(source_id_problem(GuiSource::Bucket, "myuser/my-bucket").is_none());
    }

    #[test]
    fn repo_profiles_are_always_read_only() {
        let mut profile = sample_profile();
        profile.read_only = false;
        let options = profile_mount_options(&profile).unwrap();
        assert!(options.read_only);
    }
}
