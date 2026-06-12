use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use tracing::{info, warn};
use xet_data::processing::configurations::TranslatorConfig;
use xet_data::processing::data_client::default_config;
use xet_data::processing::{CacheConfig, FileDownloadSession, create_remote_client, get_cache};
use xet_runtime::core::XetContext;

use crate::cached_xet_client::CachedXetClient;
use crate::error::{Error, Result};
use crate::file_cache::FileCache;
use crate::hub_api::{HubApiClient, HubTokenRefresher, SourceKind, parse_repo_id, split_path_prefix};
use crate::overlay::OverlayBacking;
use crate::virtual_fs::{VfsConfig, VirtualFs};
use crate::xet::{StagingDir, XetSessions};

fn setup_err(message: impl Into<String>) -> Error {
    Error::Setup(message.into())
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum CacheMode {
    /// xet-core's chunk_cache: caches xorb byte ranges on disk.
    Chunk,
    /// hf-mount's whole-file cache: caches reconstructed files keyed by xet hash.
    File,
}

#[derive(clap::Subcommand)]
pub enum Source {
    /// Mount a HuggingFace bucket (read-write by default)
    Bucket {
        /// Bucket ID, optionally with a subfolder (e.g. "user/bucket" or "user/bucket/path/to/dir")
        bucket_id: String,
        /// Local directory where the filesystem will be mounted
        mount_point: PathBuf,
    },
    /// Mount a HuggingFace repo read-only (type auto-detected from prefix)
    Repo {
        /// Repo ID, optionally with a subfolder (e.g. "user/model", "user/model/sub/dir", "datasets/user/ds/train")
        repo_id: String,
        /// Local directory where the filesystem will be mounted
        mount_point: PathBuf,
        /// Git revision to mount
        #[arg(long, default_value = "main")]
        revision: String,
    },
}

impl Source {
    pub fn mount_point(&self) -> &Path {
        match self {
            Source::Bucket { mount_point, .. } | Source::Repo { mount_point, .. } => mount_point,
        }
    }

    /// Human-readable label matching `SourceKind::Display` format.
    pub fn label(&self) -> String {
        match self {
            Source::Bucket { bucket_id, .. } => format!("bucket/{bucket_id}"),
            Source::Repo { repo_id, revision, .. } => {
                let (repo_type, parsed_id) = parse_repo_id(repo_id);
                format!("{repo_type}/{parsed_id}/{revision}")
            }
        }
    }
}

/// Mount options shared across all binaries (FUSE, NFS, daemon).
#[derive(clap::Args)]
pub struct MountOptions {
    /// HuggingFace API token (also read from HF_TOKEN env var).
    /// Required for private repos/buckets, optional for public repos.
    #[arg(long, env = "HF_TOKEN")]
    pub hf_token: Option<String>,

    /// Path to a file containing the API token. The file is re-read before
    /// each Hub request, allowing external credential managers to refresh
    /// tokens without remounting. Takes precedence over --hf-token when
    /// the file exists and is non-empty.
    #[arg(long)]
    pub token_file: Option<PathBuf>,

    /// HuggingFace Hub endpoint URL
    #[arg(long, default_value = "https://huggingface.co")]
    pub hub_endpoint: String,

    /// Directory for on-disk caches (file chunks, staging files)
    #[arg(long, default_value_os_t = default_cache_dir())]
    pub cache_dir: PathBuf,

    /// Override the UID for all files and directories (defaults to current user)
    #[arg(long)]
    pub uid: Option<u32>,

    /// Override the GID for all files and directories (defaults to current group)
    #[arg(long)]
    pub gid: Option<u32>,

    /// Mount in read-only mode (no writes allowed)
    #[arg(long, default_value_t = false)]
    pub read_only: bool,

    /// Use staging files + async flush for writes (supports random writes and seek).
    /// Default mode is append-only with synchronous close.
    #[arg(long, default_value_t = false)]
    pub advanced_writes: bool,

    /// Interval in seconds for polling remote changes (0 to disable).
    #[arg(long, default_value_t = 30)]
    pub poll_interval_secs: u64,

    /// Maximum number of concurrent tree-listing requests per poll round.
    /// Each loaded directory prefix issues one Hub API request; this cap
    /// prevents thundering-herd bursts on large mounts (e.g. transformers/docs)
    /// and is the main knob to throttle hf-mount's load on the Hub `/api`
    /// endpoint. Lower it in shared environments (e.g. Spaces) where many
    /// mounts poll in parallel.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..))]
    pub poll_listing_concurrency: u32,

    /// Maximum size in bytes for the on-disk chunk cache.
    #[arg(long, default_value_t = 10_000_000_000)]
    pub cache_size: u64,

    /// Maximum size in bytes for staging files (advanced writes).
    /// When exceeded, flushed staging files are garbage-collected to reclaim
    /// disk space. When not exceeded, staging files persist as a local cache
    /// for fast read-after-write. 0 = unlimited (no GC).
    #[arg(long, default_value_t = 0)]
    pub max_staging_size: u64,

    /// Disable the on-disk chunk cache. Every read fetches data from
    /// HF storage (no local disk caching between reads). Useful for
    /// benchmarking without cache effects.
    #[arg(long, default_value_t = false)]
    pub no_disk_cache: bool,

    /// Disk cache layer. `chunk` (default) uses xet-core's xorb-range cache;
    /// `file` uses a whole-file cache addressed by xet hash, sidestepping
    /// chunk-range fragmentation on warm reloads. The two are mutually
    /// exclusive — selecting `file` disables the chunk cache.
    #[arg(long, value_enum, default_value_t = CacheMode::Chunk)]
    pub cache_mode: CacheMode,

    /// Bypass the kernel page cache (FOPEN_DIRECT_IO). Every read goes
    /// through the FUSE handler instead of being served from cached pages.
    /// Useful for benchmarking; not recommended for production (disables
    /// efficient mmap caching).
    #[arg(long, default_value_t = false)]
    pub direct_io: bool,

    /// Kernel metadata cache TTL in milliseconds. Controls how long file
    /// attributes are trusted before re-checking via HEAD. Lower values
    /// give fresher metadata but increase latency on directory traversals
    /// (e.g. `du`, `find`, `ls -lR`) since each file lookup triggers a
    /// HEAD request after the TTL expires.
    #[arg(long, default_value_t = 10_000)]
    pub metadata_ttl_ms: u64,

    /// Always HEAD on every lookup (skip in-memory TTL cache).
    #[arg(long, default_value_t = false)]
    pub metadata_ttl_minimal: bool,

    /// Maximum number of FUSE worker threads
    #[arg(long, default_value_t = 16)]
    pub max_threads: usize,

    /// Flush debounce delay in milliseconds. After the first dirty file is
    /// enqueued, the flush batch waits this long for more writes before firing.
    #[arg(long, default_value_t = 2_000)]
    pub flush_debounce_ms: u64,

    /// Maximum flush batch window in milliseconds. A dirty file will be flushed
    /// within this time regardless of ongoing writes resetting the debounce.
    #[arg(long, default_value_t = 30_000)]
    pub flush_max_batch_window_ms: u64,

    /// Disable filtering of OS junk files (.DS_Store, Thumbs.db, etc.).
    /// By default these files are rejected on create/mkdir/rename.
    #[arg(long, default_value_t = false)]
    pub no_filter_os_files: bool,

    /// Restrict mount access to the mounting user only (FUSE only).
    /// This is the default; kept for compatibility with older command lines.
    #[arg(long, default_value_t = false, hide = true)]
    pub fuse_owner_only: bool,

    /// Allow other local users to access a FUSE mount.
    /// This restores the previous default and requires `user_allow_other` in /etc/fuse.conf on Linux.
    #[arg(long, default_value_t = false)]
    pub fuse_allow_other: bool,

    /// Permit NFS operation without enforceable local caller authorization.
    /// Needed only on platforms where AUTH_SYS + privileged source-port checks are unavailable.
    #[arg(long, default_value_t = false)]
    pub nfs_allow_unsafe_loopback: bool,

    /// Soft cap on the number of inodes kept in memory. When exceeded, a
    /// background task asks the kernel (via FUSE `notify_inval_entry`) to
    /// drop the oldest-touched dentries so `forget()` fires and we can
    /// evict them. 0 disables the evictor (unbounded growth). Recommended:
    /// set below the working set you'd see under a full-tree scrape.
    #[arg(long, default_value_t = 0)]
    pub inode_soft_limit: usize,

    /// Interval in milliseconds between LRU evictor sweeps. Only matters
    /// when `--inode-soft-limit > 0`.
    #[arg(long, default_value_t = 5_000)]
    pub lru_sweep_interval_ms: u64,

    /// Enable overlay mode. The mount point directory serves as the local
    /// layer: pre-existing local files are visible through the mount, except
    /// symlinks, which are skipped/hidden. New writes persist there in their
    /// original path layout. Reads merge local files with remote bucket or
    /// repo contents (local takes precedence). Implies --advanced-writes.
    /// Writes are never pushed to remote.
    #[arg(long, default_value_t = false)]
    pub overlay: bool,
}

/// CLI args for the foreground FUSE/NFS binaries.
#[derive(Parser)]
#[command(about = "Mount a HuggingFace bucket or repo as a filesystem", version)]
pub struct Args {
    #[command(subcommand)]
    pub source: Source,

    #[command(flatten)]
    pub options: MountOptions,
}

/// Owns a runtime and bounds its teardown. Runtime::drop waits for
/// blocking-pool tasks; a probe wedged in a stat on a dead NFS mount would
/// stall teardown forever, so detach stragglers after a grace period.
/// Implementing Drop here (not on MountSetup) keeps MountSetup's fields
/// movable.
#[derive(Default)]
struct OwnedRuntime(Option<tokio::runtime::Runtime>);

impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(5));
        }
    }
}

/// Everything needed to run a mount backend (FUSE or NFS).
pub struct MountSetup {
    pub runtime: tokio::runtime::Handle,
    /// Owned runtime, kept alive for the lifetime of this MountSetup. Empty
    /// when the runtime is owned externally (sidecar mode shares one runtime
    /// across all volumes — see `build_with_runtime`).
    _owned_runtime: OwnedRuntime,
    pub virtual_fs: Arc<VirtualFs>,
    pub mount_point: PathBuf,
    pub read_only: bool,
    pub advanced_writes: bool,
    pub direct_io: bool,
    pub metadata_ttl: std::time::Duration,
    pub max_threads: usize,
    pub metadata_ttl_ms: u64,
    pub fuse_owner_only: bool,
    pub nfs_security: NfsSecurity,
}

#[derive(Clone, Debug)]
pub struct NfsSecurity {
    pub owner_uid: u32,
    pub allow_unsafe_loopback: bool,
    pub export_name: String,
    pub filehandle_secret: [u8; 16],
}

impl NfsSecurity {
    fn new(owner_uid: u32, allow_unsafe_loopback: bool) -> Self {
        let id = uuid::Uuid::new_v4();
        Self {
            owner_uid,
            allow_unsafe_loopback,
            export_name: format!("hf-mount-{}", id.simple()),
            filehandle_secret: *id.as_bytes(),
        }
    }
}

// ── Tracing + env vars (no threads) ──────────────────────────────────

/// Initialize tracing and xet-core env vars.
/// No threads are spawned. Safe to fork() after this returns.
pub fn init_tracing(daemon: bool) {
    // Use RUST_LOG if set, otherwise default to hf_mount=info.
    let filter = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::EnvFilter::from_default_env()
    } else {
        tracing_subscriber::EnvFilter::new("hf_mount=info")
    };
    // Disable ANSI colors when daemonizing (output goes to a log file)
    // or when stderr is not a terminal.
    let ansi = !daemon && std::io::stderr().is_terminal();
    if std::env::var("RUST_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt().json().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).with_ansi(ansi).init();
    }

    // Tune xet-core for interactive FUSE reads (not batch downloads).
    for (k, v) in [
        ("HF_XET_CLIENT_AC_INITIAL_DOWNLOAD_CONCURRENCY", "16"),
        ("HF_XET_CLIENT_AC_MIN_BYTES_REQUIRED_FOR_ADJUSTMENT", "4194304"),
        ("HF_XET_RECONSTRUCTION_MIN_RECONSTRUCTION_FETCH_SIZE", "8388608"),
        ("HF_XET_RECONSTRUCTION_MIN_PREFETCH_BUFFER", "8388608"),
        ("HF_XET_RECONSTRUCTION_TARGET_BLOCK_COMPLETION_TIME", "30"),
        ("HF_XET_RECONSTRUCTION_DOWNLOAD_BUFFER_SIZE", "134217728"),
        ("HF_XET_RECONSTRUCTION_DOWNLOAD_BUFFER_LIMIT", "268435456"),
        // Raise read_timeout from 120s default so large shard uploads don't get killed
        // by the global client read_timeout before the per-request timeout kicks in.
        ("HF_XET_CLIENT_READ_TIMEOUT", "600"),
        // Upload tuning: skip slow adaptive concurrency ramp-up
        ("HF_XET_CLIENT_AC_INITIAL_UPLOAD_CONCURRENCY", "16"),
        // Larger ingestion blocks = fewer CDC calls
        ("HF_XET_DATA_INGESTION_BLOCK_SIZE", "16777216"),
    ] {
        if std::env::var(k).is_err() {
            // SAFETY: called before any threads are spawned.
            unsafe { std::env::set_var(k, v) };
        }
    }
}

// ── Build runtime + VFS (spawns threads) ─────────────────────────────

/// Build a multi-threaded tokio runtime suitable for hf-mount.
///
/// Async tasks live on the heap, so the per-thread stack only needs to fit
/// the deepest sync call. 512 KB is ample and shrinks the per-worker virtual
/// reservation from the 2 MB default.
pub fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(512 * 1024)
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}

/// Build tokio runtime, storage client, Hub client, and VFS.
/// `is_nfs` controls whether advanced writes are forced (NFS has no open/close).
///
/// Owns the runtime it creates. Use `build_with_runtime` to share one runtime
/// across multiple volumes (sidecar mode).
pub fn build(source: Source, options: MountOptions, is_nfs: bool) -> Result<MountSetup> {
    let runtime = build_runtime();
    let mut setup = build_with_runtime(source, options, is_nfs, runtime.handle().clone())?;
    setup._owned_runtime = OwnedRuntime(Some(runtime));
    Ok(setup)
}

/// Like `build`, but reuses an externally-owned runtime. The caller must keep
/// the corresponding `Runtime` alive for at least as long as the returned
/// `MountSetup`.
pub fn build_with_runtime(
    source: Source,
    options: MountOptions,
    is_nfs: bool,
    runtime: tokio::runtime::Handle,
) -> Result<MountSetup> {
    let (mount_point, source_kind, path_prefix) = match source {
        Source::Bucket { bucket_id, mount_point } => {
            let (id, prefix) =
                split_path_prefix(&bucket_id).map_err(|e| setup_err(format!("invalid bucket path: {e}")))?;
            (
                mount_point,
                SourceKind::Bucket {
                    bucket_id: id.to_string(),
                },
                prefix.to_string(),
            )
        }
        Source::Repo {
            repo_id,
            mount_point,
            revision,
        } => {
            let (repo_type, rest) = parse_repo_id(&repo_id);
            let (id, prefix) = split_path_prefix(&rest).map_err(|e| setup_err(format!("invalid repo path: {e}")))?;
            (
                mount_point,
                SourceKind::Repo {
                    repo_id: id.to_string(),
                    repo_type,
                    revision,
                },
                prefix.to_string(),
            )
        }
    };

    if options.overlay && options.read_only {
        return Err(setup_err(
            "--overlay with --read-only is pointless: overlay enables local writes, --read-only disables them. Use --read-only alone instead.",
        ));
    }
    #[cfg(windows)]
    if options.overlay {
        return Err(setup_err(
            "--overlay is not supported on Windows. Use a regular NFS mount without --overlay.",
        ));
    }

    #[cfg(not(unix))]
    let private_nfs_credentials = is_nfs && (options.hf_token.is_some() || options.token_file.is_some());
    #[cfg(not(unix))]
    if private_nfs_credentials && !options.nfs_allow_unsafe_loopback {
        return Err(setup_err(
            "credential-backed NFS mounts require --nfs-allow-unsafe-loopback on this platform because local NFS caller authorization cannot be enforced",
        ));
    }

    ensure_private_cache_dir(&options.cache_dir).map_err(|e| {
        setup_err(format!(
            "failed to prepare private cache dir {:?}: {e}",
            options.cache_dir
        ))
    })?;

    let backend = if is_nfs { "nfs" } else { "fuse" };
    let hub_client = runtime
        .block_on(HubApiClient::from_source(
            &options.hub_endpoint,
            options.hf_token.as_deref(),
            options.token_file.clone(),
            source_kind,
            path_prefix,
            backend,
        ))
        .map_err(|e| setup_err(format!("failed to initialize Hub client: {e}")))?;

    // Validate that the subfolder exists on the remote.
    if !hub_client.path_prefix().is_empty() {
        runtime.block_on(hub_client.validate_path_prefix())?;
    }

    let read_only = (options.read_only || hub_client.is_repo()) && !options.overlay;
    if hub_client.is_repo() && !options.read_only && !options.overlay {
        info!("Repo mounts are always read-only");
    }

    // Overlay: local writes allowed, but no remote write token/upload.
    let remote_read_only = read_only || options.overlay;
    let refresher = hub_client.token_refresher(remote_read_only);
    let xet_ctx = XetContext::default().map_err(|e| setup_err(format!("failed to create XetContext: {e}")))?;
    let cas_config = build_cas_config(&xet_ctx, &runtime, &refresher)?;

    // The chunk cache and the whole-file cache are mutually exclusive: when
    // `cache_mode=file` we explicitly disable xet-core's chunk_cache so we
    // don't pay disk for both layers.
    if options.cache_mode == CacheMode::File && options.no_disk_cache {
        warn!(
            "--no-disk-cache overrides --cache-mode=file: both disk caches are disabled, every read \
             will go through CAS"
        );
    }
    let file_cache = if options.cache_mode == CacheMode::File && !options.no_disk_cache {
        Some(
            FileCache::new(&options.cache_dir, options.cache_size)
                .map_err(|e| setup_err(format!("failed to create file cache: {e}")))?,
        )
    } else {
        None
    };

    let xorb_cache = if options.no_disk_cache || file_cache.is_some() {
        None
    } else {
        let xorbs_dir = options.cache_dir.join("xorbs");
        std::fs::create_dir_all(&xorbs_dir)
            .map_err(|e| setup_err(format!("failed to create xorbs dir {xorbs_dir:?}: {e}")))?;
        let config = CacheConfig {
            cache_directory: xorbs_dir,
            cache_size: options.cache_size,
        };
        Some(get_cache(&xet_ctx.config, &config).map_err(|e| setup_err(format!("failed to create chunk cache: {e}")))?)
    };

    let raw_client = runtime
        .block_on(create_remote_client(
            &cas_config,
            &uuid::Uuid::new_v4().to_string(),
            false,
        ))
        .map_err(|e| setup_err(format!("failed to create storage client: {e}")))?;
    let cached_client = CachedXetClient::new(raw_client);
    let download_session = FileDownloadSession::from_client(&xet_ctx, cached_client.clone(), xorb_cache.clone());
    let upload_config = if remote_read_only { None } else { Some(cas_config) };
    let xet_sessions = XetSessions::new(xet_ctx, download_session, upload_config, cached_client, xorb_cache);

    let advanced_writes = options.advanced_writes || options.overlay || (is_nfs && !read_only);

    // Overlay: open the mount point directory before mounting over it. The
    // handle is held by OverlayBacking so overlay-local filesystem ops stay
    // rooted at the covered directory after mount.
    let overlay_backing = if options.overlay {
        std::fs::create_dir_all(&mount_point)
            .map_err(|e| setup_err(format!("failed to create mount point {mount_point:?} for overlay: {e}")))?;
        Some(
            OverlayBacking::open_dir(&mount_point)
                .map_err(|e| setup_err(format!("failed to open mount point {mount_point:?} for overlay: {e}")))?,
        )
    } else {
        None
    };

    // Repos need a staging dir for HTTP download cache (open_readonly),
    // even when advanced_writes is disabled.
    let staging_dir = if advanced_writes || hub_client.is_repo() {
        Some(StagingDir::new(&options.cache_dir, options.max_staging_size)?)
    } else {
        None
    };

    let uid = options.uid.unwrap_or_else(default_uid);
    let gid = options.gid.unwrap_or_else(default_gid);

    // Ignore EEXIST: the directory may already exist from a previous (possibly
    // stale) mount. FUSE/NFS will fail at mount time if it's actually busy.
    //
    // On Windows, drive-letter NFS targets such as `Z:` or `Z:\` are created
    // by mount.exe itself. Calling create_dir_all on them before the drive is
    // mapped fails and prevents the mount command from ever running.
    if should_create_mount_point(&mount_point, is_nfs)
        && let Err(e) = std::fs::create_dir_all(&mount_point)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(setup_err(format!("failed to create mount point {mount_point:?}: {e}")));
    }

    if is_nfs && options.direct_io {
        info!("--direct-io is ignored for NFS mounts (no NFS equivalent)");
    }

    let backend_name = if is_nfs { "nfs" } else { "fuse" };
    let subfolder_info = if hub_client.path_prefix().is_empty() {
        String::new()
    } else {
        format!(" (subfolder: {})", hub_client.path_prefix())
    };
    let access_mode = if options.overlay {
        "overlay: remote read-only, local writes enabled"
    } else if read_only {
        "read-only"
    } else {
        "read-write"
    };
    info!(
        "Mounting {}{} at {:?} ({}, backend={})",
        hub_client.source(),
        subfolder_info,
        mount_point,
        access_mode,
        backend_name,
    );
    info!(
        "Config: advanced_writes={} overlay={} remote_read_only={} direct_io={} poll_interval={}s \
         poll_listing_concurrency={} metadata_ttl={}ms \
         cache_dir={:?} cache_size={} no_disk_cache={} cache_mode={:?} max_staging_size={} max_threads={} \
         flush_debounce={}ms flush_max_batch={}ms uid={} gid={} filter_os_files={}",
        advanced_writes,
        options.overlay,
        remote_read_only,
        options.direct_io,
        options.poll_interval_secs,
        options.poll_listing_concurrency,
        options.metadata_ttl_ms,
        options.cache_dir,
        options.cache_size,
        options.no_disk_cache,
        options.cache_mode,
        options.max_staging_size,
        options.max_threads,
        options.flush_debounce_ms,
        options.flush_max_batch_window_ms,
        uid,
        gid,
        !options.no_filter_os_files,
    );

    let metadata_ttl = std::time::Duration::from_millis(options.metadata_ttl_ms);

    let virtual_fs = VirtualFs::new(
        runtime.clone(),
        hub_client,
        xet_sessions,
        staging_dir,
        file_cache,
        overlay_backing,
        VfsConfig {
            read_only,
            advanced_writes,
            uid,
            gid,
            poll_interval_secs: options.poll_interval_secs,
            poll_listing_concurrency: options.poll_listing_concurrency as usize,
            metadata_ttl,
            serve_lookup_from_cache: !options.metadata_ttl_minimal,
            filter_os_files: !options.no_filter_os_files,
            direct_io: options.direct_io && !is_nfs,
            flush_debounce: std::time::Duration::from_millis(options.flush_debounce_ms),
            flush_max_batch_window: std::time::Duration::from_millis(options.flush_max_batch_window_ms),
            // NFS clients use inode numbers as stable file IDs; evicting an
            // inode the client still holds would surface as NFS3ERR_STALE on
            // its next RPC. The eviction safety hooks (forget / inval_entry)
            // only exist on the FUSE side, so force the limit off here.
            inode_soft_limit: if is_nfs { 0 } else { options.inode_soft_limit },
            lru_sweep_interval: std::time::Duration::from_millis(options.lru_sweep_interval_ms),
        },
    );

    Ok(MountSetup {
        runtime,
        _owned_runtime: OwnedRuntime::default(),
        virtual_fs,
        mount_point,
        read_only,
        advanced_writes,
        direct_io: options.direct_io,
        metadata_ttl,
        max_threads: options.max_threads,
        metadata_ttl_ms: options.metadata_ttl_ms,
        fuse_owner_only: effective_fuse_owner_only(&options),
        nfs_security: NfsSecurity::new(uid, options.nfs_allow_unsafe_loopback),
    })
}

fn effective_fuse_owner_only(options: &MountOptions) -> bool {
    options.fuse_owner_only || !options.fuse_allow_other
}

// ── Combined entry point (foreground binaries) ──────────────────────

/// Parse CLI args, build VFS and all dependencies.
/// `is_nfs` controls whether advanced writes are forced (NFS has no open/close).
/// Exits the process with an error message when setup fails.
pub fn setup(is_nfs: bool) -> MountSetup {
    raise_fd_limit();
    let args = Args::parse();
    init_tracing(false);
    build(args.source, args.options, is_nfs).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    })
}

/// Try to raise the soft file descriptor limit to avoid "Too many open files"
/// errors during large batch operations. Most FUSE/NFS filesystems do this.
pub fn raise_fd_limit() {
    #[cfg(unix)]
    {
        const TARGET_NOFILE: u64 = 65536;
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: rlim is a plain C struct, getrlimit/setrlimit are standard POSIX.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) } != 0 || rlim.rlim_cur >= TARGET_NOFILE {
            return;
        }
        rlim.rlim_cur = TARGET_NOFILE.min(rlim.rlim_max);
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) } != 0 {
            eprintln!("warning: failed to raise file descriptor limit to {TARGET_NOFILE}");
        }
    }
}

pub fn default_cache_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(base) = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")) {
            return PathBuf::from(base).join("hf-mount").join("cache");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library").join("Caches").join("hf-mount");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(base).join("hf-mount");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".cache").join("hf-mount");
        }
    }
    PathBuf::from(".hf-mount-cache")
}

fn ensure_private_cache_dir(path: &Path) -> std::io::Result<()> {
    ensure_private_dir(path)
}

fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.file_type().is_symlink()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "path must not be a symlink",
            ));
        }

        std::fs::create_dir_all(path)?;
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "path must be an owner-private directory, not a symlink or file",
            ));
        }

        let uid = default_uid();
        if meta.uid() != uid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("directory is owned by uid {}, expected {}", meta.uid(), uid),
            ));
        }

        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(mode & !0o077);
            std::fs::set_permissions(path, perms)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

fn should_create_mount_point(mount_point: &Path, is_nfs: bool) -> bool {
    #[cfg(windows)]
    {
        if is_nfs && is_windows_drive_mount_point(mount_point) {
            return false;
        }
    }
    #[cfg(not(windows))]
    let _ = (mount_point, is_nfs);
    true
}

#[cfg(windows)]
fn is_windows_drive_mount_point(path: &Path) -> bool {
    crate::windows::drive_letter(&path.as_os_str().to_string_lossy()).is_some()
}

#[cfg(unix)]
fn default_uid() -> u32 {
    // SAFETY: getuid is a thread-safe POSIX call with no preconditions.
    unsafe { libc::getuid() }
}

#[cfg(unix)]
fn default_gid() -> u32 {
    // SAFETY: getgid is a thread-safe POSIX call with no preconditions.
    unsafe { libc::getgid() }
}

#[cfg(not(unix))]
fn default_uid() -> u32 {
    0
}

#[cfg(not(unix))]
fn default_gid() -> u32 {
    0
}

fn build_cas_config(
    ctx: &XetContext,
    runtime: &tokio::runtime::Handle,
    refresher: &Arc<HubTokenRefresher>,
) -> Result<Arc<TranslatorConfig>> {
    let jwt = runtime
        .block_on(refresher.fetch_initial())
        .map_err(|e| setup_err(format!("failed to get storage token: {e}")))?;
    info!("Got storage token for endpoint: {}", jwt.cas_url);
    let config = default_config(
        ctx,
        jwt.cas_url,
        Some((jwt.access_token, jwt.exp)),
        Some(refresher.clone()),
        None,
    )
    .map_err(|e| setup_err(format!("failed to build TranslatorConfig: {e}")))?;
    Ok(Arc::new(config))
}

#[cfg(all(test, windows))]
mod windows_mount_point_tests {
    use super::*;

    #[test]
    fn nfs_drive_letters_are_not_created_as_directories() {
        assert!(!should_create_mount_point(Path::new("Z:"), true));
        assert!(!should_create_mount_point(Path::new("Z:\\"), true));
        assert!(should_create_mount_point(Path::new("C:\\hf-mounts\\repo"), true));
        assert!(should_create_mount_point(Path::new("Z:"), false));
    }
}

#[cfg(all(test, unix))]
mod storage_security_tests {
    use super::*;
    use clap::Parser;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn cache_dir_permissions_are_hardened() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_cache_dir(&cache).unwrap();

        let mode = std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn cache_dir_rejects_symlink_root() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("cache-link");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let err = ensure_private_cache_dir(&link).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn fuse_owner_only_is_default_with_allow_other_opt_in() {
        let default_args = Args::parse_from(["hf-mount-fuse", "repo", "openai/gpt", "/tmp/hf-mount-test"]);
        assert!(effective_fuse_owner_only(&default_args.options));

        let allow_args = Args::parse_from([
            "hf-mount-fuse",
            "--fuse-allow-other",
            "repo",
            "openai/gpt",
            "/tmp/hf-mount-test",
        ]);
        assert!(!effective_fuse_owner_only(&allow_args.options));
    }
}
