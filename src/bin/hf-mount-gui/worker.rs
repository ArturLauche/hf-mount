//! Detached background worker: the `--background-worker` process entry point,
//! the status-file IPC between worker and GUI, and the poller thread that
//! watches worker state without ever blocking the UI thread.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use hf_mount::nfs::NfsMountEvent;
use serde::{Deserialize, Serialize};

use crate::platform;
use crate::profile::{load_mount_profile, profile_mount_options, profile_mount_source};
use crate::util::{app_config_dir, current_unix_secs, panic_message, write_file_replace};

pub const BACKGROUND_WORKER_ARG: &str = "--background-worker";
const WORKER_STATUS_STALE_AFTER_SECS: u64 = 120;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerState {
    Mounting,
    Mounted,
    Stopping,
    Stopped,
    Failed,
}

impl WorkerState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            WorkerState::Mounting | WorkerState::Mounted | WorkerState::Stopping
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub state: WorkerState,
    pub headline: String,
    pub detail: String,
    #[serde(default)]
    pub mount_point: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub updated_at_secs: u64,
}

// ── Status file IPC ───────────────────────────────────────────────────

pub fn worker_status_path() -> Result<PathBuf, String> {
    Ok(app_config_dir()?.join("background-status.json"))
}

pub fn worker_log_path() -> Result<PathBuf, String> {
    Ok(app_config_dir()?.join("background.log"))
}

pub fn read_worker_status() -> Result<Option<WorkerStatus>, String> {
    let path = worker_status_path()?;
    // The worker replaces the file atomically, but a read can still race the
    // (non-atomic) replace on Windows — retry briefly before giving up.
    for attempt in 0..3 {
        if !path.exists() {
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            return Ok(None);
        }

        let bytes = std::fs::read(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        match serde_json::from_slice(&bytes) {
            Ok(status) => return Ok(Some(status)),
            Err(_) if attempt < 2 => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("Failed to parse {}: {e}", path.display())),
        }
    }

    Ok(None)
}

fn write_worker_status(
    state: WorkerState,
    headline: impl Into<String>,
    detail: impl Into<String>,
    mount_point: Option<&Path>,
    pid: Option<u32>,
) -> Result<(), String> {
    let path = worker_status_path()?;
    let status = WorkerStatus {
        state,
        headline: headline.into(),
        detail: detail.into(),
        mount_point: mount_point.map(|mount_point| mount_point.to_string_lossy().into_owned()),
        pid,
        updated_at_secs: current_unix_secs(),
    };
    let json = serde_json::to_vec_pretty(&status).map_err(|e| format!("Failed to serialize worker status: {e}"))?;
    write_file_replace(&path, &json)
}

pub fn clear_worker_status() {
    if let Ok(path) = worker_status_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Overwrite the status file after the GUI terminates a worker that never
/// reached `Mounted`, so later launches don't see a stale `Mounting` claim.
pub fn mark_worker_stopped(mount_point: Option<&Path>) {
    let _ = write_worker_status(
        WorkerState::Stopped,
        "Background mount stopped",
        "The worker was stopped before the mount completed.",
        mount_point,
        None,
    );
}

/// Whether `pid` is alive and still identifies as an hf-mount background
/// worker (guards against PID reuse after a crash or reboot).
pub fn worker_process_matches(pid: u32) -> bool {
    platform::worker_process_alive(pid, BACKGROUND_WORKER_ARG)
}

/// Whether a reported worker status corresponds to something actually alive:
/// its process exists and identifies as our worker, its mount answers, or the
/// heartbeat is recent. Blocking (process probes, filesystem stat) — poller
/// thread only.
pub fn worker_status_is_live(status: &WorkerStatus, mount_point: Option<&Path>) -> bool {
    if status.pid.is_some_and(worker_process_matches) {
        return true;
    }

    if status.state == WorkerState::Mounted && mount_point.is_some_and(platform::mount_point_appears_active) {
        return true;
    }

    status.updated_at_secs != 0
        && current_unix_secs().saturating_sub(status.updated_at_secs) <= WORKER_STATUS_STALE_AFTER_SECS
}

// ── Worker log ────────────────────────────────────────────────────────

fn append_worker_log(message: impl AsRef<str>) {
    let Ok(mut file) = worker_log_file() else {
        return;
    };
    let _ = writeln!(file, "{}", message.as_ref());
}

fn worker_log_file() -> Result<std::fs::File, String> {
    let path = worker_log_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            let mut perms = file
                .metadata()
                .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?
                .permissions();
            perms.set_mode(0o600);
            file.set_permissions(perms)
                .map_err(|e| format!("Failed to chmod {}: {e}", path.display()))?;
        }
    }
    Ok(file)
}

// ── Spawning ──────────────────────────────────────────────────────────

/// Launch a detached `--background-worker` process for the saved profile.
pub fn spawn_background_worker(mount_point: &Path) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not locate current executable: {e}"))?;
    clear_worker_status();
    write_worker_status(
        WorkerState::Mounting,
        "Background worker launching",
        "Starting detached process.",
        Some(mount_point),
        None,
    )?;
    let result = (|| {
        let log = worker_log_file()?;
        let log_for_stderr = log
            .try_clone()
            .map_err(|e| format!("Failed to duplicate background log handle: {e}"))?;
        let mut command = Command::new(exe);
        command
            .arg(BACKGROUND_WORKER_ARG)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_for_stderr));
        platform::detach_command(&mut command);
        command
            .spawn()
            .map_err(|e| format!("Failed to launch background worker: {e}"))
    })();
    if result.is_err() {
        // Remove the provisional "launching" status so the poller doesn't
        // treat a worker that never existed as live for the staleness window.
        clear_worker_status();
    }
    result
}

// ── Worker process entry point ────────────────────────────────────────

/// Body of the detached `--background-worker` process: load the saved
/// profile, run the NFS mount, and mirror progress into the status file.
pub fn run_background_worker() -> Result<(), String> {
    crate::util::init_backend_once();

    append_worker_log("Background worker starting");
    let profile = load_mount_profile()?.ok_or_else(|| "No saved mount settings were found.".to_string())?;
    let source = profile_mount_source(&profile)?;
    let options = profile_mount_options(&profile)?;
    let worker_mount_point = source.mount_point().to_path_buf();
    let mount_label = worker_mount_point.display().to_string();
    write_worker_status(
        WorkerState::Mounting,
        "Background worker starting",
        format!("Target: {mount_label}"),
        Some(&worker_mount_point),
        Some(std::process::id()),
    )?;

    // catch_unwind is a last resort for panics deep inside the backend; setup
    // and mount errors arrive as plain Results.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let setup = hf_mount::setup::build(source, options, true).map_err(|e| e.to_string())?;
        let virtual_fs = setup.virtual_fs.clone();
        let mount_point = setup.mount_point.clone();
        let params = hf_mount::nfs::NfsMountParams {
            metadata_ttl_ms: setup.metadata_ttl_ms,
            read_only: setup.read_only,
            security: setup.nfs_security.clone(),
            shutdown: None,
        };
        let event_mount_point = mount_point.clone();
        setup
            .runtime
            .block_on(hf_mount::nfs::mount_nfs_with_callback(
                virtual_fs,
                &mount_point,
                params,
                None,
                move |event| handle_background_mount_event(&event_mount_point, event),
            ))
            .map_err(|e| e.to_string())
    }));

    match result {
        Ok(Ok(())) => {
            append_worker_log("Background worker stopped cleanly");
            write_worker_status(
                WorkerState::Stopped,
                "Background mount stopped",
                "The background worker exited cleanly.",
                Some(&worker_mount_point),
                Some(std::process::id()),
            )?;
            Ok(())
        }
        Ok(Err(message)) => {
            append_worker_log(format!("Mount failed: {message}"));
            let _ = write_worker_status(
                WorkerState::Failed,
                "Mount failed",
                &message,
                Some(&worker_mount_point),
                Some(std::process::id()),
            );
            Err(message)
        }
        Err(payload) => {
            let message = panic_message(payload);
            append_worker_log(format!("Mount crashed: {message}"));
            let _ = write_worker_status(
                WorkerState::Failed,
                "Mount crashed",
                &message,
                Some(&worker_mount_point),
                Some(std::process::id()),
            );
            Err(message)
        }
    }
}

fn handle_background_mount_event(default_mount_point: &Path, event: NfsMountEvent) {
    match event {
        NfsMountEvent::ServerListening { port } => {
            append_worker_log(format!("Local NFS server is listening on 127.0.0.1:{port}"));
            let _ = write_worker_status(
                WorkerState::Mounting,
                "Local NFS server is listening",
                format!("127.0.0.1:{port}"),
                Some(default_mount_point),
                Some(std::process::id()),
            );
        }
        NfsMountEvent::MountCommand { command } => {
            append_worker_log(format!("Running {command}"));
        }
        NfsMountEvent::Mounted { mount_point } => {
            append_worker_log(format!("Mounted at {mount_point}"));
            let event_mount_point = Path::new(&mount_point);
            let _ = write_worker_status(
                WorkerState::Mounted,
                "Mounted",
                format!("Mounted at {mount_point}"),
                Some(event_mount_point),
                Some(std::process::id()),
            );
        }
        NfsMountEvent::ShuttingDown { reason } => {
            append_worker_log(format!("Shutting down: {reason}"));
            let _ = write_worker_status(
                WorkerState::Stopping,
                "Shutting down",
                reason,
                Some(default_mount_point),
                Some(std::process::id()),
            );
        }
    }
}

// ── Poller thread ─────────────────────────────────────────────────────

/// A poll result: the parsed status file plus a liveness verdict.
#[derive(Clone, Debug)]
pub struct WorkerSnapshot {
    /// Bumped on every completed poll so the UI can skip stale reads.
    pub generation: u64,
    pub status: Option<WorkerStatus>,
    pub live: bool,
    pub error: Option<String>,
}

/// Watches the background worker from a dedicated thread so the UI thread
/// never touches the status file, `tasklist.exe`, or a possibly-wedged NFS
/// mount. The previous implementation did all of that on every frame.
pub struct WorkerPoller {
    snapshot: Arc<Mutex<WorkerSnapshot>>,
    stop: Arc<AtomicBool>,
    wake: Arc<std::sync::Condvar>,
    wake_lock: Arc<Mutex<()>>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerPoller {
    /// Start polling. `repaint` is invoked after every poll so the UI wakes
    /// up promptly.
    pub fn start(repaint: impl Fn() + Send + 'static) -> Self {
        let snapshot = Arc::new(Mutex::new(WorkerSnapshot {
            generation: 0,
            status: None,
            live: false,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(std::sync::Condvar::new());
        let wake_lock = Arc::new(Mutex::new(()));

        let thread_snapshot = snapshot.clone();
        let thread_stop = stop.clone();
        let thread_wake = wake.clone();
        let thread_wake_lock = wake_lock.clone();
        let handle = std::thread::Builder::new()
            .name("worker-status-poller".to_string())
            .spawn(move || {
                let mut generation = 0u64;
                while !thread_stop.load(Ordering::SeqCst) {
                    let (status, live, error) = match read_worker_status() {
                        Ok(Some(status)) => {
                            // Liveness probes (process lookup, mount stat) are
                            // only worth their cost while the file claims an
                            // active worker; terminal states are simply dead.
                            let live = status.state.is_active() && {
                                let mount_point = status
                                    .mount_point
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|mount_point| !mount_point.is_empty())
                                    .map(PathBuf::from);
                                worker_status_is_live(&status, mount_point.as_deref())
                            };
                            (Some(status), live, None)
                        }
                        Ok(None) => (None, false, None),
                        Err(e) => (None, false, Some(e)),
                    };

                    {
                        let mut shared = thread_snapshot.lock().expect("worker snapshot poisoned");
                        generation += 1;
                        *shared = WorkerSnapshot {
                            generation,
                            status,
                            live,
                            error,
                        };
                    }
                    repaint();

                    let guard = thread_wake_lock.lock().expect("poller wake lock poisoned");
                    let _unused = thread_wake
                        .wait_timeout(guard, POLL_INTERVAL)
                        .expect("poller wake lock poisoned");
                }
            })
            .expect("failed to spawn worker poller thread");

        Self {
            snapshot,
            stop,
            wake,
            wake_lock,
            handle: Some(handle),
        }
    }

    pub fn snapshot(&self) -> WorkerSnapshot {
        self.snapshot.lock().expect("worker snapshot poisoned").clone()
    }
}

impl Drop for WorkerPoller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        {
            let _guard = self.wake_lock.lock().expect("poller wake lock poisoned");
            self.wake.notify_all();
        }
        // The poller may be blocked inside a liveness probe on a wedged NFS
        // mount; joining unconditionally would hang window close. Reap it if
        // it winds down promptly, otherwise detach — the process is exiting
        // and the thread holds only Arcs.
        if let Some(handle) = self.handle.take() {
            let deadline = std::time::Instant::now() + Duration::from_millis(250);
            while !handle.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}
