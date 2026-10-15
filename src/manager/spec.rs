//! What to mount and where: the validated, serialisable description shared
//! by the CLI, the desktop app, saved profiles and the supervisor.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hub_api::{RepoType, split_path_prefix};

/// Kind of Hugging Face storage being mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Model,
    Dataset,
    Space,
    Bucket,
}

impl SourceType {
    pub fn is_repo(self) -> bool {
        !matches!(self, SourceType::Bucket)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::Model => "model",
            SourceType::Dataset => "dataset",
            SourceType::Space => "space",
            SourceType::Bucket => "bucket",
        }
    }

    fn repo_type(self) -> Option<RepoType> {
        match self {
            SourceType::Model => Some(RepoType::Model),
            SourceType::Dataset => Some(RepoType::Dataset),
            SourceType::Space => Some(RepoType::Space),
            SourceType::Bucket => None,
        }
    }
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Optional engine tuning; `None` means "use the default".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MountTuning {
    /// Root of the on-disk cache (a per-source subdirectory is used).
    pub cache_dir: Option<PathBuf>,
    /// Maximum on-disk chunk cache size in bytes.
    pub cache_size_bytes: Option<u64>,
    /// Disable the on-disk chunk cache entirely.
    pub no_disk_cache: bool,
    /// How long file metadata is trusted before re-checking the Hub.
    pub metadata_ttl_ms: Option<u64>,
    /// Interval for polling remote changes (0 disables).
    pub poll_interval_secs: Option<u64>,
    /// Hub endpoint, default `https://huggingface.co`.
    pub hub_endpoint: Option<String>,
}

/// A validated mount request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountSpec {
    pub source_type: SourceType,
    /// `namespace/name` (repos may also use a legacy single-segment alias
    /// such as `gpt2`, which the Hub resolves).
    pub id: String,
    /// Subfolder inside the repo/bucket to expose as the drive root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subfolder: Option<String>,
    /// Git revision for repositories (branch, tag or commit); `None` = `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Drive letter, normalised to `X:`.
    pub target: String,
    /// Repositories are always read-only; buckets are read-write unless set.
    pub read_only: bool,
    #[serde(default)]
    pub tuning: MountTuning,
}

/// A validation failure, phrased for end users.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError(pub String);

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpecError {}

fn err(msg: impl Into<String>) -> SpecError {
    SpecError(msg.into())
}

/// Hub naming rules for a namespace or repo/bucket name segment.
pub fn validate_name_segment(segment: &str, what: &str) -> Result<(), SpecError> {
    if segment.is_empty() {
        return Err(err(format!("{what} must not be empty")));
    }
    if segment == "." || segment == ".." {
        return Err(err("ids must not contain '.' or '..' segments"));
    }
    if segment.len() > 96 {
        return Err(err(format!("{what} {segment:?} is longer than 96 characters")));
    }
    if let Some(c) = segment
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(err(format!("{what} {segment:?} contains an invalid character {c:?}")));
    }
    if segment.starts_with(['-', '.']) || segment.ends_with(['-', '.']) {
        return Err(err(format!("{what} {segment:?} must not start or end with '-' or '.'")));
    }
    if segment.contains("--") || segment.contains("..") {
        return Err(err(format!("{what} {segment:?} must not contain '--' or '..'")));
    }
    Ok(())
}

/// Validate a subfolder path inside a repo or bucket (`a/b/c`).
pub fn validate_subfolder(path: &str) -> Result<String, SpecError> {
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Err(err("subfolder must not be empty"));
    }
    if trimmed.contains('\\') {
        return Err(err("subfolder uses '/' as separator, not '\\'"));
    }
    for component in trimmed.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(err(format!(
                "subfolder {path:?} contains an empty, '.' or '..' component"
            )));
        }
        if let Some(c) = component
            .chars()
            .find(|c| c.is_control() || matches!(c, '?' | '#' | ':'))
        {
            return Err(err(format!("subfolder {path:?} contains an invalid character {c:?}")));
        }
    }
    Ok(trimmed.to_string())
}

/// Normalise a drive letter (`h`, `h:`, `H:\`) to `H:`. Only D–Z are offered
/// for mounts (A/B are legacy floppy letters, C is the system drive).
pub fn normalize_target(input: &str) -> Result<String, SpecError> {
    let trimmed = input.trim().trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let (Some(letter), rest) = (chars.next(), chars.as_str()) else {
        return Err(err("a drive letter such as H: is required"));
    };
    if !(rest.is_empty() || rest == ":") || !letter.is_ascii_alphabetic() {
        return Err(err(format!(
            "{input:?} is not a drive letter; use a free letter such as H: (directory mount points are not supported)"
        )));
    }
    let letter = letter.to_ascii_uppercase();
    if letter < 'D' {
        return Err(err(format!("{letter}: is reserved; choose a letter from D: to Z:")));
    }
    Ok(format!("{letter}:"))
}

impl MountSpec {
    /// Build a spec from the CLI-style triple `(kind, id, target)`.
    ///
    /// `kind` is `repo` (type taken from a `datasets/` or `spaces/` prefix,
    /// as in upstream hf-mount), `model`, `dataset`, `space` or `bucket`.
    /// The id may carry a subfolder after its second segment
    /// (`user/model/sub/dir`), like upstream.
    pub fn parse(kind: &str, id: &str, target: &str) -> Result<Self, SpecError> {
        let id = id.trim();
        let (source_type, rest) = match kind.to_ascii_lowercase().as_str() {
            "repo" => {
                if let Some(rest) = id.strip_prefix("datasets/") {
                    (SourceType::Dataset, rest)
                } else if let Some(rest) = id.strip_prefix("spaces/") {
                    (SourceType::Space, rest)
                } else if let Some(rest) = id.strip_prefix("models/") {
                    (SourceType::Model, rest)
                } else {
                    (SourceType::Model, id)
                }
            }
            "model" | "models" => (SourceType::Model, id),
            "dataset" | "datasets" => (SourceType::Dataset, id.strip_prefix("datasets/").unwrap_or(id)),
            "space" | "spaces" => (SourceType::Space, id.strip_prefix("spaces/").unwrap_or(id)),
            "bucket" | "buckets" => (SourceType::Bucket, id),
            other => {
                return Err(err(format!(
                    "unknown source type {other:?}; use repo, model, dataset, space or bucket"
                )));
            }
        };
        let (base, prefix) = split_path_prefix(rest).map_err(|e| err(format!("invalid id {id:?}: {e}")))?;
        let spec = MountSpec {
            source_type,
            id: base.to_string(),
            subfolder: (!prefix.is_empty()).then(|| prefix.to_string()),
            revision: None,
            target: normalize_target(target)?,
            read_only: source_type.is_repo(),
            tuning: MountTuning::default(),
        };
        spec.validated()
    }

    /// Check every field and return a normalised copy.
    pub fn validated(&self) -> Result<Self, SpecError> {
        let mut spec = self.clone();
        let segments: Vec<&str> = spec.id.split('/').collect();
        match (spec.source_type, segments.as_slice()) {
            (SourceType::Bucket, [ns, name]) => {
                validate_name_segment(ns, "namespace")?;
                validate_name_segment(name, "bucket name")?;
            }
            (SourceType::Bucket, _) => return Err(err("bucket ids look like namespace/bucket-name")),
            (_, [name]) => validate_name_segment(name, "repository name")?,
            (_, [ns, name]) => {
                validate_name_segment(ns, "namespace")?;
                validate_name_segment(name, "repository name")?;
            }
            _ => return Err(err("repository ids look like namespace/name")),
        }
        if let Some(sub) = &spec.subfolder {
            spec.subfolder = Some(validate_subfolder(sub)?);
        }
        if let Some(rev) = &spec.revision {
            if !spec.source_type.is_repo() {
                return Err(err("--revision only applies to model, dataset and space repositories"));
            }
            crate::setup::validate_revision(rev).map_err(err)?;
        }
        spec.target = normalize_target(&spec.target)?;
        if spec.source_type.is_repo() {
            spec.read_only = true;
        }
        if let Some(endpoint) = &spec.tuning.hub_endpoint
            && !(endpoint.starts_with("https://") || endpoint.starts_with("http://localhost"))
        {
            return Err(err("hub endpoint must be an https:// URL"));
        }
        Ok(spec)
    }

    pub fn revision_or_default(&self) -> &str {
        self.revision.as_deref().unwrap_or("main")
    }

    /// The id as upstream hf-mount's `Source` expects it (type prefix and
    /// subfolder included), e.g. `datasets/user/ds/train`.
    pub fn upstream_id(&self) -> String {
        let prefix = match self.source_type {
            SourceType::Dataset => "datasets/",
            SourceType::Space => "spaces/",
            SourceType::Model | SourceType::Bucket => "",
        };
        match &self.subfolder {
            Some(sub) => format!("{prefix}{}/{sub}", self.id),
            None => format!("{prefix}{}", self.id),
        }
    }

    /// Human-readable source, e.g. `dataset user/ds@main/train`.
    pub fn display_source(&self) -> String {
        let mut s = format!("{} {}", self.source_type, self.id);
        if self.source_type.is_repo() {
            s.push('@');
            s.push_str(self.revision_or_default());
        }
        if let Some(sub) = &self.subfolder {
            s.push_str(" /");
            s.push_str(sub);
        }
        s
    }

    /// Stable, filesystem-safe key for per-source cache directories
    /// (`models--user--name`, like the huggingface_hub cache).
    pub fn cache_key(&self) -> String {
        let kind = match self.source_type {
            SourceType::Model => "models",
            SourceType::Dataset => "datasets",
            SourceType::Space => "spaces",
            SourceType::Bucket => "buckets",
        };
        format!("{kind}--{}", self.id.replace('/', "--"))
    }

    /// Volume label for File Explorer (≤ 32 chars): the repo/bucket name.
    pub fn volume_label(&self) -> String {
        let name = self.id.rsplit('/').next().unwrap_or(&self.id);
        name.chars().take(32).collect()
    }

    /// The repo type, for repositories.
    pub fn repo_type(&self) -> Option<RepoType> {
        self.source_type.repo_type()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upstream_cli_forms() {
        let s = MountSpec::parse("repo", "openai-community/gpt2", "H:").unwrap();
        assert_eq!(s.source_type, SourceType::Model);
        assert_eq!(s.id, "openai-community/gpt2");
        assert_eq!(s.target, "H:");
        assert!(s.read_only);
        assert_eq!(s.upstream_id(), "openai-community/gpt2");

        let s = MountSpec::parse("repo", "datasets/username/dataset-name", "d").unwrap();
        assert_eq!(s.source_type, SourceType::Dataset);
        assert_eq!(s.id, "username/dataset-name");
        assert_eq!(s.target, "D:");
        assert_eq!(s.upstream_id(), "datasets/username/dataset-name");

        let s = MountSpec::parse("bucket", "username/my-bucket", "x:\\").unwrap();
        assert_eq!(s.source_type, SourceType::Bucket);
        assert!(!s.read_only);
        assert_eq!(s.target, "X:");

        let s = MountSpec::parse("repo", "user/model/sub/dir/", "H:").unwrap();
        assert_eq!(s.id, "user/model");
        assert_eq!(s.subfolder.as_deref(), Some("sub/dir"));
        assert_eq!(s.upstream_id(), "user/model/sub/dir");

        let s = MountSpec::parse("dataset", "datasets/user/ds", "Z:").unwrap();
        assert_eq!(s.id, "user/ds");

        // Legacy alias, resolved by the Hub.
        assert_eq!(MountSpec::parse("model", "gpt2", "H:").unwrap().id, "gpt2");
    }

    #[test]
    fn rejects_bad_ids() {
        for (kind, id) in [
            ("bucket", "only-one-segment"),
            ("repo", "user/../etc"),
            ("repo", "user/na me"),
            ("repo", "user/-leading"),
            ("repo", "user/double--dash"),
            ("repo", "user/name?x=1"),
            ("repo", ""),
            ("unknown", "a/b"),
            ("repo", "user/model/../../x"),
            ("repo", "user/model/sub\\dir"),
        ] {
            assert!(
                MountSpec::parse(kind, id, "H:").is_err(),
                "{kind} {id:?} should be rejected"
            );
        }
    }

    #[test]
    fn targets_must_be_free_style_drive_letters() {
        assert_eq!(normalize_target("h").unwrap(), "H:");
        assert_eq!(normalize_target(" z:/ ").unwrap(), "Z:");
        for bad in ["", "C:", "a:", "1:", "HH", "H:\\mnt", "\\\\srv\\share", "/mnt/hf"] {
            assert!(normalize_target(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn revision_rules() {
        let mut s = MountSpec::parse("repo", "user/model", "H:").unwrap();
        s.revision = Some("v1.0".into());
        assert!(s.validated().is_ok());
        s.revision = Some("main?x".into());
        assert!(s.validated().is_err());
        s.revision = Some("../x".into());
        assert!(s.validated().is_err());
        let mut b = MountSpec::parse("bucket", "user/b", "H:").unwrap();
        b.revision = Some("main".into());
        assert!(b.validated().is_err(), "buckets have no revisions");
    }

    #[test]
    fn repos_are_forced_read_only() {
        let mut s = MountSpec::parse("repo", "user/model", "H:").unwrap();
        s.read_only = false;
        assert!(s.validated().unwrap().read_only);
    }

    #[test]
    fn display_and_keys() {
        let mut s = MountSpec::parse("repo", "datasets/user/ds/train", "H:").unwrap();
        s.revision = Some("v2".into());
        assert_eq!(s.display_source(), "dataset user/ds@v2 /train");
        assert_eq!(s.cache_key(), "datasets--user--ds");
        assert_eq!(s.volume_label(), "ds");
        let json = serde_json::to_string(&s).unwrap();
        let back: MountSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn endpoint_must_be_https() {
        let mut s = MountSpec::parse("repo", "user/model", "H:").unwrap();
        s.tuning.hub_endpoint = Some("http://evil.example".into());
        assert!(s.validated().is_err());
        s.tuning.hub_endpoint = Some("https://huggingface.co".into());
        assert!(s.validated().is_ok());
    }
}
