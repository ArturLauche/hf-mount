//! [`Mounter`] implementation backed by WinFsp and the upstream engine.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tracing::{error, info, warn};

use super::drives;
use super::winfsp_fs::{MountConfig, MountError, WinFspMount};
use crate::manager::config::{Settings, recovered_dir};
use crate::manager::errors::classify_setup_error;
use crate::manager::protocol::{Credential, ErrorCode, ErrorInfo, MountStats, UnmountReport};
use crate::manager::registry::{MountHandle, Mounter};
use crate::manager::spec::MountSpec;
use crate::setup::{Backend, MountOptions, MountSetup, Source, try_build_with_runtime};
use crate::winfs::names;

/// How long startup keeps retrying transient Hub failures before reporting.
const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
/// How long `FlushFileBuffers` waits for one file's upload.
const FLUSH_WAIT: Duration = Duration::from_secs(120);

#[derive(Parser)]
struct OptionsOnly {
    #[command(flatten)]
    options: MountOptions,
}

/// Upstream's defaults for every engine option, without reading the
/// process environment for secrets.
fn default_options() -> MountOptions {
    let mut options = OptionsOnly::try_parse_from(["hf-mount"])
        .expect("defaults always parse")
        .options;
    // `--hf-token` has `env = "HF_TOKEN"`; the supervisor must only use the
    // credential the client sent, never its own environment.
    options.hf_token = None;
    options.token_file = None;
    options
}

pub struct WinFspMounter {
    runtime: tokio::runtime::Handle,
    settings: Settings,
}

impl WinFspMounter {
    pub fn new(runtime: tokio::runtime::Handle, settings: Settings) -> Self {
        Self { runtime, settings }
    }

    fn options_for(&self, spec: &MountSpec, credential: Option<&Credential>, cache_dir: PathBuf) -> MountOptions {
        let mut o = default_options();
        if let Some(endpoint) = spec
            .tuning
            .hub_endpoint
            .clone()
            .or_else(|| self.settings.hub_endpoint.clone())
        {
            o.hub_endpoint = endpoint;
        }
        match credential {
            Some(Credential::Token { token, .. }) => o.hf_token = Some(token.clone()),
            Some(Credential::TokenFile { path }) => o.token_file = Some(path.clone()),
            None => {}
        }
        o.cache_dir = cache_dir;
        o.cache_size = spec.tuning.cache_size_bytes.unwrap_or(self.settings.cache_size_bytes);
        o.no_disk_cache = spec.tuning.no_disk_cache;
        o.read_only = spec.read_only;
        o.metadata_ttl_ms = spec.tuning.metadata_ttl_ms.unwrap_or(self.settings.metadata_ttl_ms);
        if let Some(poll) = spec.tuning.poll_interval_secs {
            o.poll_interval_secs = poll;
        }
        o.flush_shutdown_timeout_ms = self.settings.upload_wait_secs.saturating_mul(1000);
        o
    }
}

/// Lock a per-source cache directory for the life of a mount. A second
/// concurrent mount of the same source (in any process) gets `<dir>-2`, …,
/// so two chunk-cache instances never share a directory.
fn lock_cache_dir(base: &Path) -> std::io::Result<(PathBuf, File)> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    for n in 1..=16 {
        let dir = if n == 1 {
            base.to_path_buf()
        } else {
            PathBuf::from(format!("{}-{n}", base.display()))
        };
        std::fs::create_dir_all(&dir)?;
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(dir.join(".hf-mount.lock"))
        {
            Ok(lock) => return Ok((dir, lock)),
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many concurrent mounts of the same source"))
}

pub fn classify_mount_error(err: &MountError) -> ErrorInfo {
    match err {
        MountError::Driver(e) => ErrorInfo::new(ErrorCode::DriverMissing, e.to_string())
            .with_hint("install WinFsp (see `hf-mount doctor`), then retry"),
        MountError::DriverUnavailable(_) => ErrorInfo::new(ErrorCode::DriverUnavailable, err.to_string()),
        MountError::TargetInUse(t) => ErrorInfo::new(ErrorCode::TargetInUse, format!("drive {t} is already in use"))
            .with_hint("choose another letter (`hf-mount drives` lists free ones)"),
        MountError::InvalidMountPoint(_) => ErrorInfo::new(ErrorCode::InvalidArgument, err.to_string()),
        MountError::Security(_) | MountError::WinFsp { .. } => ErrorInfo::new(ErrorCode::Internal, err.to_string()),
    }
}

impl Mounter for WinFspMounter {
    fn target_available(&self, target: &str) -> bool {
        target.chars().next().is_some_and(drives::is_letter_free)
    }

    fn mount(&self, spec: &MountSpec, credential: Option<&Credential>) -> Result<Box<dyn MountHandle>, ErrorInfo> {
        // Fail fast on a missing driver, before any network round trip.
        super::winfsp::api().map_err(|e| classify_mount_error(&MountError::Driver(e)))?;

        let cache_root = spec
            .tuning
            .cache_dir
            .clone()
            .unwrap_or_else(|| self.settings.cache_root());
        let (cache_dir, cache_lock) = lock_cache_dir(&cache_root.join(spec.cache_key())).map_err(|e| {
            ErrorInfo::new(
                ErrorCode::Internal,
                format!("cache directory {}: {e}", cache_root.display()),
            )
            .with_hint("check that the cache directory is writable")
        })?;
        let options = self.options_for(spec, credential, cache_dir.clone());
        let mount_point = PathBuf::from(&spec.target);
        let source = if spec.source_type.is_repo() {
            Source::Repo {
                repo_id: spec.upstream_id(),
                mount_point,
                revision: spec.revision_or_default().to_string(),
            }
        } else {
            Source::Bucket {
                bucket_id: spec.upstream_id(),
                mount_point,
            }
        };
        let metadata_ttl = Duration::from_millis(options.metadata_ttl_ms);
        let rt = self.runtime.clone();
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            try_build_with_runtime(source, options, Backend::WinFsp, rt, STARTUP_DEADLINE)
        }))
        .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "mount setup panicked (see the supervisor log)"))?;
        let setup = built.map_err(|e| classify_setup_error(&e, spec, credential.is_some()))?;

        let config = MountConfig {
            mount_point: spec.target.clone(),
            volume_label: spec.volume_label(),
            read_only: setup.read_only,
            case_insensitive_fallback: setup.read_only,
            file_info_timeout: metadata_ttl,
            dispatcher_threads: 0,
            flush_wait_timeout: FLUSH_WAIT,
            volume_serial: volume_serial(spec),
            debug_log: std::env::var_os("HF_MOUNT_WINFSP_DEBUG").is_some(),
        };
        let mount = match WinFspMount::mount(setup.virtual_fs.clone(), self.runtime.clone(), config) {
            Ok(mount) => mount,
            Err(e) => {
                setup.virtual_fs.shutdown();
                return Err(classify_mount_error(&e));
            }
        };
        let resolved = match &setup.path_prefix {
            p if p.is_empty() => setup.source.to_string(),
            p => format!("{}/{p}", setup.source),
        };
        info!("mounted {resolved} at {} (cache {})", spec.target, cache_dir.display());
        Ok(Box::new(WinMountHandle {
            target: spec.target.clone(),
            resolved,
            mount: Some(mount),
            setup: Some(setup),
            _cache_lock: cache_lock,
        }))
    }
}

fn volume_serial(spec: &MountSpec) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spec.target.hash(&mut h);
    spec.id.hash(&mut h);
    h.finish() as u32
}

struct WinMountHandle {
    target: String,
    resolved: String,
    mount: Option<WinFspMount>,
    setup: Option<MountSetup>,
    _cache_lock: File,
}

impl MountHandle for WinMountHandle {
    fn resolved_source(&self) -> Option<String> {
        Some(self.resolved.clone())
    }

    fn stats(&self) -> MountStats {
        let adapter = self.mount.as_ref().map(|m| m.stats()).unwrap_or_default();
        let uploads = self
            .setup
            .as_ref()
            .map(|s| s.virtual_fs.pending_uploads())
            .unwrap_or_default();
        MountStats {
            open_handles: adapter.open_handles,
            open_write_handles: adapter.open_write_handles,
            bytes_read: adapter.bytes_read,
            bytes_written: adapter.bytes_written,
            failed_operations: adapter.failed_ops,
            pending_uploads: uploads.dirty_files,
            pending_deletes: uploads.pending_deletes,
            failed_uploads: uploads.failed_files,
            last_error: adapter.last_error,
        }
    }

    fn lost(&self) -> Option<String> {
        self.mount
            .as_ref()
            .filter(|m| m.stopped_abnormally())
            .map(|_| "WinFsp stopped serving the volume (driver unloaded or uninstalled)".to_string())
    }

    fn retry_uploads(&self) -> usize {
        self.setup.as_ref().map(|s| s.virtual_fs.requeue_dirty()).unwrap_or(0)
    }

    fn unmount(mut self: Box<Self>) -> UnmountReport {
        let mut report = UnmountReport {
            target: self.target.clone(),
            ..Default::default()
        };
        // 1. Drive letter gone, no new I/O.
        if let Some(mount) = &self.mount {
            mount.unmount();
        }
        // 2. Drain uploads (bounded by the configured upload wait).
        if let Some(setup) = &self.setup {
            let vfs = setup.virtual_fs.clone();
            vfs.shutdown();
            let remaining = vfs.dirty_files();
            if !remaining.is_empty() {
                report.unflushed_files = remaining.len();
                match preserve(&self.target, &remaining) {
                    Ok(dir) => {
                        report.warnings.push(format!(
                            "{} file(s) could not be uploaded before unmount; copies were saved to {}",
                            remaining.len(),
                            dir.display()
                        ));
                        report.recovered_to = Some(dir);
                    }
                    Err(e) => {
                        error!("could not preserve unflushed files: {e}");
                        report.warnings.push(format!(
                            "{} file(s) could not be uploaded and could not be saved locally: {e}",
                            remaining.len()
                        ));
                    }
                }
            }
        }
        // 3. Free the volume, then the engine (which deletes its staging dir).
        drop(self.mount.take());
        drop(self.setup.take());
        report
    }
}

impl Drop for WinMountHandle {
    fn drop(&mut self) {
        if self.mount.is_some() || self.setup.is_some() {
            warn!("mount {} dropped without unmount; releasing", self.target);
            if let Some(setup) = &self.setup {
                setup.virtual_fs.shutdown();
            }
        }
    }
}

/// Copy staging files of uncommitted writes to the recovery folder,
/// preserving their remote paths (made Windows-safe).
fn preserve(target: &str, files: &[(String, PathBuf)]) -> std::io::Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dir = recovered_dir().join(format!("{stamp}-{}", target.trim_end_matches(':')));
    for (remote, staging) in files {
        let mut dest = dir.clone();
        for component in remote.split('/').filter(|c| !c.is_empty()) {
            dest.push(safe_component(component));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(staging, &dest)?;
    }
    Ok(dir)
}

fn safe_component(name: &str) -> String {
    if names::windows_name_problem(name).is_none() {
        return name.to_string();
    }
    let mut s: String = name
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() || names::is_reserved_device_name(&s) {
        s.insert(0, '_');
    }
    s
}

/// Shared, cheap handle to a mounter for the registry.
pub fn mounter(runtime: tokio::runtime::Handle, settings: Settings) -> Arc<dyn Mounter> {
    Arc::new(WinFspMounter::new(runtime, settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_ignore_environment_tokens() {
        // SAFETY: test-local env mutation; no concurrent reader of HF_TOKEN in this test.
        unsafe { std::env::set_var("HF_TOKEN", "hf_from_environment_should_be_ignored") };
        let o = default_options();
        unsafe { std::env::remove_var("HF_TOKEN") };
        assert!(o.hf_token.is_none());
        assert_eq!(o.hub_endpoint, "https://huggingface.co");
        assert_eq!(o.cache_size, 10_000_000_000);
    }

    #[test]
    fn safe_components() {
        assert_eq!(safe_component("ok.txt"), "ok.txt");
        assert_eq!(safe_component("a:b"), "a_b");
        assert_eq!(safe_component("trail. "), "trail");
        assert_eq!(safe_component("CON"), "_CON");
    }

    #[test]
    fn cache_dirs_are_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("models--a--b");
        let (first, _l1) = lock_cache_dir(&base).unwrap();
        let (second, _l2) = lock_cache_dir(&base).unwrap();
        assert_eq!(first, base);
        assert_ne!(first, second);
        drop(_l1);
        let (again, _l3) = lock_cache_dir(&base).unwrap();
        assert_eq!(again, base);
    }
}
