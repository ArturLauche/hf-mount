//! Application state and frame layout: sidebar navigation, central tab body,
//! bottom status bar. Mount control (start/stop) and background-worker
//! synchronization live here; the tab bodies are in `*_tab.rs`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use hf_mount::nfs::{MountShutdown, NfsMountEvent};
use hf_mount::setup::{MountOptions, Source, default_cache_dir};

use crate::platform;
use crate::preflight::{CheckItem, CheckLevel, run_preflight_checks, summarize_checks};
use crate::profile::{
    GuiSource, MountProfile, RecentSource, load_mount_profile, profile_mount_options, profile_mount_source,
    save_mount_profile, source_id_problem,
};
use crate::theme::*;
use crate::util::{current_env_hf_token, format_elapsed, optional_text, panic_message};
use crate::widgets::{nav_item, status_chip};
use crate::worker::{WorkerPoller, WorkerSnapshot, WorkerStatus, spawn_background_worker};

const MAX_LOG_LINES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Mount,
    Activity,
    Setup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountState {
    Ready,
    Mounting,
    Mounted,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Error,
}

impl LogLevel {
    /// Stable token used when serializing log lines (e.g. Copy log).
    pub fn as_token(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Error => "ERROR",
        }
    }
}

/// One session-log line: wall-clock timestamp, severity, message. Structured
/// at insert time so the Activity tab renders without per-frame formatting.
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub time: String,
    pub level: LogLevel,
    pub text: String,
}

impl LogEntry {
    fn new(level: LogLevel, text: String) -> Self {
        Self {
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            level,
            text,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SharedStatus {
    /// Bumped on every mutation. The UI thread re-clones this struct only when
    /// the revision moved, instead of cloning the whole log every frame.
    pub revision: u64,
    pub state: MountState,
    pub headline: String,
    pub detail: String,
    pub log: VecDeque<LogEntry>,
}

impl Default for SharedStatus {
    fn default() -> Self {
        let mut log = VecDeque::new();
        log.push_back(LogEntry::new(LogLevel::Info, "Ready".to_string()));
        Self {
            revision: 1,
            state: MountState::Ready,
            headline: "Ready".to_string(),
            detail: "Configure a source and start the mount.".to_string(),
            log,
        }
    }
}

pub type SharedMountStatus = Arc<Mutex<SharedStatus>>;

fn level_for_state(state: MountState) -> LogLevel {
    match state {
        MountState::Failed => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

pub fn set_status(
    status: &SharedMountStatus,
    state: MountState,
    headline: impl Into<String>,
    detail: impl Into<String>,
) {
    let headline = headline.into();
    let detail = detail.into();
    let mut status = status.lock().expect("status mutex poisoned");
    status.state = state;
    status.headline = headline.clone();
    status.detail = detail.clone();
    push_log_locked(&mut status, level_for_state(state), format!("{headline}: {detail}"));
}

pub fn set_status_if_changed(
    status: &SharedMountStatus,
    state: MountState,
    headline: impl Into<String>,
    detail: impl Into<String>,
) {
    let headline = headline.into();
    let detail = detail.into();
    let mut status = status.lock().expect("status mutex poisoned");
    if status.state == state && status.headline == headline && status.detail == detail {
        return;
    }
    status.state = state;
    status.headline = headline.clone();
    status.detail = detail.clone();
    push_log_locked(&mut status, level_for_state(state), format!("{headline}: {detail}"));
}

pub fn push_log(status: &SharedMountStatus, message: impl Into<String>) {
    push_log_with_level(status, LogLevel::Info, message);
}

pub fn push_log_with_level(status: &SharedMountStatus, level: LogLevel, message: impl Into<String>) {
    let mut status = status.lock().expect("status mutex poisoned");
    push_log_locked(&mut status, level, message.into());
}

fn push_log_locked(status: &mut SharedStatus, level: LogLevel, message: String) {
    status.revision = status.revision.wrapping_add(1);
    status.log.push_back(LogEntry::new(level, message));
    while status.log.len() > MAX_LOG_LINES {
        status.log.pop_front();
    }
}

pub struct MountGuiApp {
    // Form state.
    pub source: GuiSource,
    pub source_id: String,
    pub revision: String,
    pub mount_point: String,
    pub hf_token: String,
    pub show_token: bool,
    pub token_file: String,
    pub hub_endpoint: String,
    pub cache_dir: String,
    pub read_only: bool,
    pub run_in_background: bool,
    pub nfs_allow_unsafe_loopback: bool,
    pub cache_size_gb: u64,
    pub poll_interval_secs: u64,
    pub metadata_ttl_ms: u64,
    pub read_fetch_timeout_ms: u64,
    pub autostart_enabled: bool,
    pub show_advanced: bool,
    pub recent_sources: Vec<RecentSource>,

    // UI state.
    pub tab: Tab,
    pub checks: Vec<CheckItem>,
    /// Frame-local copy of the shared status, refreshed only when the shared
    /// revision moves — the UI never re-clones an unchanged log.
    status_cache: SharedStatus,

    // Mount state.
    pub status: SharedMountStatus,
    mount_thread: Option<JoinHandle<()>>,
    mount_shutdown: Option<MountShutdown>,
    stop_thread: Option<JoinHandle<()>>,
    background_child: Option<Child>,
    pub active_background: bool,
    pub active_mount_point: Option<PathBuf>,
    /// Identity of the source captured when the active mount started;
    /// independent of the editable form fields.
    pub active_source_label: Option<String>,
    mounted_since: Option<Instant>,
    worker_poller: Option<WorkerPoller>,
    last_worker_generation: u64,
    /// Last worker status applied from the poller, used to tell genuinely new
    /// worker reports from a stale file being re-read every poll.
    last_worker_status: Option<WorkerStatus>,
    /// Set when the user intentionally terminated the background worker, so
    /// the reaper doesn't report the kill as a failure.
    background_stop_requested: bool,
}

impl MountGuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mount_point = platform::default_mount_point();
        let checks = run_preflight_checks(&mount_point);
        // Cache and shared status must start from the same snapshot: the
        // revision-gated refresh assumes equal revision implies equal content.
        let initial_status = SharedStatus::default();
        let mut app = Self {
            source: GuiSource::Repo,
            source_id: "openai-community/gpt2".to_string(),
            revision: "main".to_string(),
            mount_point,
            hf_token: std::env::var("HF_TOKEN").unwrap_or_default(),
            show_token: false,
            token_file: String::new(),
            hub_endpoint: "https://huggingface.co".to_string(),
            cache_dir: default_cache_dir().to_string_lossy().into_owned(),
            read_only: true,
            run_in_background: false,
            nfs_allow_unsafe_loopback: false,
            cache_size_gb: crate::profile::DEFAULT_CACHE_SIZE_GB,
            poll_interval_secs: crate::profile::DEFAULT_POLL_INTERVAL_SECS,
            metadata_ttl_ms: crate::profile::DEFAULT_METADATA_TTL_MS,
            read_fetch_timeout_ms: crate::profile::DEFAULT_READ_FETCH_TIMEOUT_MS,
            autostart_enabled: crate::autostart::autostart_is_enabled(),
            show_advanced: false,
            recent_sources: Vec::new(),
            tab: Tab::Mount,
            checks,
            status_cache: initial_status.clone(),
            status: Arc::new(Mutex::new(initial_status)),
            mount_thread: None,
            mount_shutdown: None,
            stop_thread: None,
            background_child: None,
            active_background: false,
            active_mount_point: None,
            active_source_label: None,
            mounted_since: None,
            worker_poller: None,
            last_worker_generation: 0,
            last_worker_status: None,
            background_stop_requested: false,
        };

        match load_mount_profile() {
            Ok(Some(profile)) => {
                app.apply_profile(profile);
                app.checks = run_preflight_checks(&app.mount_point);
            }
            Ok(None) => {}
            Err(e) => push_log_with_level(
                &app.status,
                LogLevel::Error,
                format!("Could not load saved settings: {e}"),
            ),
        }

        // Reconcile any existing background worker synchronously before the
        // first frame, so opening a second GUI while a mount is already running
        // gates Start immediately instead of racing the async poller and
        // spawning a duplicate worker on the same status file.
        app.reconcile_existing_worker();

        // One always-on poller watches the background worker status file from
        // its own thread; the UI thread only ever reads its snapshot.
        let repaint_ctx = cc.egui_ctx.clone();
        app.worker_poller = Some(WorkerPoller::start(move || repaint_ctx.request_repaint()));
        app
    }

    fn apply_profile(&mut self, profile: MountProfile) {
        self.source = profile.source;
        self.source_id = profile.source_id;
        self.revision = profile.revision;
        self.mount_point = profile.mount_point;
        self.token_file = profile.token_file;
        self.hub_endpoint = profile.hub_endpoint;
        self.cache_dir = profile.cache_dir;
        self.read_only = profile.read_only || self.source == GuiSource::Repo;
        self.run_in_background = profile.run_in_background;
        self.nfs_allow_unsafe_loopback = profile.nfs_allow_unsafe_loopback;
        self.cache_size_gb = profile.cache_size_gb;
        self.poll_interval_secs = profile.poll_interval_secs;
        self.metadata_ttl_ms = profile.metadata_ttl_ms;
        self.read_fetch_timeout_ms = profile.read_fetch_timeout_ms;
        self.recent_sources = profile.recent_sources;
    }

    fn profile(&self) -> MountProfile {
        MountProfile {
            source: self.source,
            source_id: self.source_id.clone(),
            revision: self.revision.clone(),
            mount_point: self.mount_point.clone(),
            token_file: self.token_file.clone(),
            hub_endpoint: self.hub_endpoint.clone(),
            cache_dir: self.cache_dir.clone(),
            read_only: self.source == GuiSource::Repo || self.read_only,
            run_in_background: self.run_in_background,
            nfs_allow_unsafe_loopback: self.nfs_allow_unsafe_loopback,
            cache_size_gb: self.cache_size_gb,
            poll_interval_secs: self.poll_interval_secs,
            metadata_ttl_ms: self.metadata_ttl_ms,
            read_fetch_timeout_ms: self.read_fetch_timeout_ms,
            recent_sources: self.recent_sources.clone(),
        }
    }

    fn save_profile(&self) -> Result<(), String> {
        save_mount_profile(&self.profile())
    }

    pub fn apply_recent_source(&mut self, recent: &RecentSource) {
        self.source = recent.source;
        self.source_id = recent.source_id.clone();
        if recent.source == GuiSource::Repo {
            self.revision = if recent.revision.is_empty() {
                "main".to_string()
            } else {
                recent.revision.clone()
            };
            self.read_only = true;
        }
    }

    pub fn apply_autostart_setting(&mut self) {
        let requested = self.autostart_enabled;
        if requested && let Err(e) = self.save_profile() {
            self.autostart_enabled = false;
            set_status(&self.status, MountState::Failed, "Could not save settings", e);
            return;
        }

        match crate::autostart::set_autostart_enabled(requested) {
            Ok(()) => {
                push_log(
                    &self.status,
                    if requested {
                        "Autostart enabled: the saved mount starts at login"
                    } else {
                        "Autostart disabled: the login startup entry was removed"
                    },
                );
            }
            Err(e) => {
                self.autostart_enabled = !requested;
                set_status(&self.status, MountState::Failed, "Could not update autostart", e);
            }
        }
    }

    pub fn refresh_checks(&mut self) {
        self.checks = run_preflight_checks(&self.mount_point);
        push_log(&self.status, summarize_checks(&self.checks));
    }

    /// Refresh the frame-local status cache if the shared status changed.
    /// Called once per frame before drawing; the lock is held only long enough
    /// to compare revisions (and clone when they differ).
    fn refresh_status_cache(&mut self) {
        let shared = self.status.lock().expect("status mutex poisoned");
        if shared.revision != self.status_cache.revision {
            self.status_cache = shared.clone();
        }
    }

    /// Frame-local snapshot of the shared status. Cheap — no lock, no clone.
    pub fn current_status(&self) -> &SharedStatus {
        &self.status_cache
    }

    /// Read the live shared state directly (single small lock). For control
    /// paths that must observe status written earlier in the same frame.
    pub fn live_state(&self) -> MountState {
        self.status.lock().expect("status mutex poisoned").state
    }

    /// Seconds since the current mount reached `Mounted`, if it is mounted.
    pub fn mounted_uptime_secs(&self) -> Option<u64> {
        self.mounted_since.map(|since| since.elapsed().as_secs())
    }

    pub fn is_mount_running(&self) -> bool {
        self.active_background
            || self.background_child.is_some()
            || self.mount_thread.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    pub fn is_stopping(&self) -> bool {
        self.stop_thread.is_some() || self.mount_shutdown.as_ref().is_some_and(MountShutdown::is_requested)
    }

    pub fn first_blocking_check(&self) -> Option<&CheckItem> {
        self.checks
            .iter()
            .find(|check| check.label == "Client for NFS" && check.level == CheckLevel::Fail)
            .or_else(|| self.checks.iter().find(|check| check.level == CheckLevel::Fail))
    }

    pub fn source_problem(&self) -> Option<&'static str> {
        source_id_problem(self.source, &self.source_id)
    }

    // ── Mount control ─────────────────────────────────────────────────

    pub fn start_mount(&mut self) {
        let checks = run_preflight_checks(&self.mount_point);
        let failure = checks.iter().find(|check| check.level == CheckLevel::Fail).cloned();
        self.checks = checks;
        if let Some(failure) = failure {
            set_status(
                &self.status,
                MountState::Failed,
                format!("Setup check failed: {}", failure.label),
                failure.detail,
            );
            return;
        }

        push_log(&self.status, summarize_checks(&self.checks));
        let profile = self.profile();
        let source = match profile_mount_source(&profile) {
            Ok(source) => source,
            Err(e) => {
                set_status(&self.status, MountState::Failed, "Invalid source", e);
                return;
            }
        };
        let options = match self.mount_options(&profile) {
            Ok(options) => options,
            Err(e) => {
                set_status(&self.status, MountState::Failed, "Invalid mount options", e);
                return;
            }
        };
        let mount_point = source.mount_point().to_path_buf();
        let mount_label = mount_point.display().to_string();
        let source_label = source.label();

        let inline_token = optional_text(&self.hf_token);
        if self.run_in_background
            && optional_text(&self.token_file).is_none()
            && inline_token.is_some()
            && inline_token != current_env_hf_token()
        {
            set_status(
                &self.status,
                MountState::Failed,
                "Token not available to background worker",
                "Set HF_TOKEN before launching the GUI or provide a token file.",
            );
            return;
        }

        self.remember_recent_source();
        if let Err(e) = self.save_profile() {
            set_status(&self.status, MountState::Failed, "Could not save settings", e);
            return;
        }

        set_status(
            &self.status,
            MountState::Mounting,
            "Preparing mount",
            format!("Target: {mount_label}"),
        );

        if self.run_in_background {
            self.background_stop_requested = false;
            match spawn_background_worker(&mount_point, &source_label) {
                Ok(child) => {
                    self.background_child = Some(child);
                    self.active_background = true;
                    self.active_mount_point = Some(mount_point);
                    self.active_source_label = Some(source_label);
                    set_status(
                        &self.status,
                        MountState::Mounting,
                        "Background mount starting",
                        format!("Target: {mount_label}"),
                    );
                }
                Err(e) => {
                    set_status(&self.status, MountState::Failed, "Could not start background mount", e);
                }
            }
            return;
        }

        self.active_background = false;
        self.active_mount_point = Some(mount_point);
        self.active_source_label = Some(source_label);
        let shutdown = MountShutdown::new();
        self.mount_shutdown = Some(shutdown.clone());
        let shared_status = self.status.clone();
        self.mount_thread = Some(thread::spawn(move || {
            run_mount(source, options, shared_status, shutdown)
        }));
    }

    fn remember_recent_source(&mut self) {
        let entry = RecentSource {
            source: self.source,
            source_id: self.source_id.trim().to_string(),
            revision: self.revision.trim().to_string(),
        };
        let mut profile = self.profile();
        profile.remember_recent(entry);
        self.recent_sources = profile.recent_sources;
    }

    fn mount_options(&self, profile: &MountProfile) -> Result<MountOptions, String> {
        let mut options = profile_mount_options(profile)?;
        if let Some(token) = optional_text(&self.hf_token) {
            options.hf_token = Some(token);
        }
        Ok(options)
    }

    pub fn stop_mount(&mut self) {
        // Foreground mounts stop through the cooperative shutdown handle: the
        // backend unmounts itself and the thread winds down. Works during
        // `Mounting` too, unlike an external unmount command.
        if self.mount_thread.as_ref().is_some_and(|handle| !handle.is_finished())
            && let Some(shutdown) = &self.mount_shutdown
        {
            shutdown.request();
            set_status(
                &self.status,
                MountState::Stopping,
                "Stop requested",
                "Waiting for the mount to shut down.",
            );
            return;
        }

        // A background worker that has not mounted yet cannot be stopped by
        // unmounting — there is nothing mounted, and the worker would carry
        // on and mount anyway. Terminate the worker process instead.
        if self.active_background && !self.worker_reported_mounted() {
            self.stop_unmounted_background_worker();
            return;
        }

        // Mounted targets are stopped by unmounting; the worker notices the
        // mount disappearing and exits. The unmount command can block on a
        // wedged NFS mount, so it runs on its own thread.
        let Some(mount_point) = self.active_mount_point.clone() else {
            set_status(
                &self.status,
                MountState::Failed,
                "No active mount",
                "There is no recorded mount point to unmount.",
            );
            return;
        };

        if self.stop_thread.is_some() {
            return; // A stop is already in flight.
        }

        set_status(
            &self.status,
            MountState::Stopping,
            "Unmount requested",
            format!("Target: {}", mount_point.display()),
        );

        let status = self.status.clone();
        self.stop_thread = Some(thread::spawn(move || {
            if let Err(e) = platform::unmount_path(&mount_point) {
                set_status(&status, MountState::Failed, "Unmount failed", e);
            }
        }));
    }

    fn worker_reported_mounted(&self) -> bool {
        self.last_worker_status
            .as_ref()
            .is_some_and(|status| status.state == crate::worker::WorkerState::Mounted)
    }

    /// One-shot synchronous reconcile of an already-running background worker
    /// at startup, before the async poller's first snapshot. Seeds the tracking
    /// state so the first frame correctly reflects (and gates Start on) a live
    /// worker. Blocking — only called once during construction.
    fn reconcile_existing_worker(&mut self) {
        let Ok(Some(status)) = crate::worker::read_worker_status() else {
            return;
        };
        if !status.state.is_active() {
            return;
        }
        // Non-blocking heuristic only. The full liveness check stats the mount
        // (and probes the process table), either of which can wedge on a dead
        // NFS mount — that must never run before the first frame or the window
        // never opens. Optimistically adopt a fresh-heartbeat worker so Start is
        // gated immediately; the poller re-confirms on its own thread within one
        // interval and clears this if the worker is actually gone.
        if crate::worker::worker_status_heartbeat_fresh(&status) {
            self.active_background = true;
            self.active_mount_point = worker_mount_point(&status);
            self.active_source_label = status.source_label.clone();
            set_status_if_changed(
                &self.status,
                worker_mount_state(&status),
                status.headline.clone(),
                status.detail.clone(),
            );
            self.last_worker_status = Some(status);
            push_log(&self.status, "Reconnected to existing background worker");
        }
    }

    fn stop_unmounted_background_worker(&mut self) {
        // A pid from our own spawned child is trustworthy. One read back from
        // the status file may have been recycled by another process — verify
        // it still identifies as our worker before signaling anything.
        let child_pid = self.background_child.as_ref().map(Child::id).filter(|pid| *pid != 0);
        let status_pid = self
            .last_worker_status
            .as_ref()
            .and_then(|status| status.pid)
            .filter(|pid| *pid != 0);

        let pid = match (child_pid, status_pid) {
            (Some(pid), _) => pid,
            (None, Some(pid)) if crate::worker::worker_process_matches(pid) => pid,
            (None, Some(_)) => {
                // The recorded pid no longer belongs to a worker: it is dead.
                // Clear the stale record instead of killing a stranger.
                crate::worker::mark_worker_stopped(self.active_mount_point.as_deref());
                self.active_background = false;
                self.active_mount_point = None;
                self.active_source_label = None;
                set_status(
                    &self.status,
                    MountState::Stopped,
                    "Background mount stopped",
                    "No live worker process was found; cleared the stale record.",
                );
                return;
            }
            (None, None) => {
                set_status(
                    &self.status,
                    MountState::Failed,
                    "Could not stop background worker",
                    "The worker process id is not known yet; try again in a moment.",
                );
                return;
            }
        };

        if let Err(e) = platform::terminate_process(pid) {
            set_status(&self.status, MountState::Failed, "Could not stop background worker", e);
            return;
        }

        self.background_stop_requested = true;
        crate::worker::mark_worker_stopped(self.active_mount_point.as_deref());

        // The worker may have mounted in the window between the last status
        // poll and the kill — clean up any mount it managed to create. Track
        // the cleanup in `stop_thread` (not detached) so window close waits for
        // it (bounded) rather than killing it mid-unmount and orphaning a mount.
        if let Some(mount_point) = self.active_mount_point.clone()
            && self.stop_thread.is_none()
        {
            let status = self.status.clone();
            self.stop_thread = Some(thread::spawn(move || {
                // Only clean up a mount we actually own. The worker may have
                // mounted in the race window before the kill, but the configured
                // path could equally be an unrelated pre-existing mount that we
                // must not detach — confirm it is our loopback NFS export first.
                if platform::mount_point_is_ours(&mount_point)
                    && let Err(e) = platform::unmount_path(&mount_point)
                {
                    push_log_with_level(
                        &status,
                        LogLevel::Error,
                        format!("Post-stop cleanup unmount failed: {e}"),
                    );
                }
            }));
        }

        self.active_background = false;
        self.active_mount_point = None;
        self.active_source_label = None;
        set_status(
            &self.status,
            MountState::Stopped,
            "Background mount stopped",
            "The worker was stopped before the mount completed.",
        );
    }

    pub fn open_active_mount(&mut self) {
        match platform::open_mount_point(self.active_mount_point.as_deref()) {
            Ok(()) => push_log(&self.status, "Opened mount point"),
            Err(e) => set_status(&self.status, MountState::Failed, "Could not open mount point", e),
        }
    }

    // ── Per-frame housekeeping ────────────────────────────────────────

    fn collect_finished(&mut self) {
        self.consume_worker_snapshot();
        self.collect_background_child();
        self.collect_mount_thread();
        self.collect_stop_thread();
        self.track_mounted_since();
    }

    fn consume_worker_snapshot(&mut self) {
        let Some(snapshot) = self.worker_poller.as_ref().map(WorkerPoller::snapshot) else {
            return;
        };
        if snapshot.generation == 0 || snapshot.generation == self.last_worker_generation {
            return;
        }
        let first = self.last_worker_generation == 0;
        self.last_worker_generation = snapshot.generation;
        self.apply_worker_snapshot(snapshot, first);
    }

    fn apply_worker_snapshot(&mut self, snapshot: WorkerSnapshot, first: bool) {
        if let Some(error) = &snapshot.error {
            push_log_with_level(
                &self.status,
                LogLevel::Error,
                format!("Could not read background status: {error}"),
            );
            return;
        }

        let Some(status) = snapshot.status else {
            self.last_worker_status = None;
            if self.active_background {
                self.active_background = false;
                self.active_mount_point = None;
                self.active_source_label = None;
                set_status(
                    &self.status,
                    MountState::Failed,
                    "Background worker unavailable",
                    "No background status file was found.",
                );
            }
            return;
        };

        // The poller re-reads the file every interval; only treat the report
        // as news when its content actually changed since the last apply.
        let status_changed = self.last_worker_status.as_ref() != Some(&status);
        self.last_worker_status = Some(status.clone());

        let active_state = status.state.is_active();
        if active_state && snapshot.live {
            let newly_connected = !self.active_background;
            self.active_background = true;
            if let Some(mount_point) = worker_mount_point(&status) {
                self.active_mount_point = Some(mount_point);
            }
            if let Some(label) = &status.source_label {
                self.active_source_label = Some(label.clone());
            }
            if newly_connected && first {
                push_log(&self.status, "Reconnected to background worker");
            }
        } else {
            if active_state && (self.active_background || first) {
                set_status(
                    &self.status,
                    MountState::Failed,
                    "Background worker unavailable",
                    "Saved background state is stale; start the mount again.",
                );
            }
            if self.active_background {
                self.active_background = false;
                self.background_child = None;
                if self.mount_thread.is_none() {
                    self.active_mount_point = None;
                    self.active_source_label = None;
                }
            }
        }

        // Mirror the worker's reported status. Never clobber a live
        // foreground mount, and only mirror terminal states when the report
        // is new (or at startup) — otherwise a stale Stopped/Failed file
        // would keep overwriting newer local status every poll interval.
        let foreground_active = self.mount_thread.as_ref().is_some_and(|handle| !handle.is_finished());
        let terminal_report = !active_state && (status_changed || first);
        if !foreground_active && (self.active_background || terminal_report) {
            set_status_if_changed(
                &self.status,
                worker_mount_state(&status),
                status.headline,
                status.detail,
            );
        }
    }

    fn collect_background_child(&mut self) {
        let background_result = self.background_child.as_mut().map(Child::try_wait).transpose();
        match background_result {
            Ok(Some(Some(exit_status))) => {
                self.background_child = None;
                self.active_background = false;
                self.active_mount_point = None;
                self.active_source_label = None;
                // The worker's own status file usually carries a more specific
                // message; only fall back to the exit code when it didn't.
                let current = self.live_state();
                if exit_status.success() {
                    if !matches!(current, MountState::Stopped | MountState::Failed) {
                        set_status(
                            &self.status,
                            MountState::Stopped,
                            "Background mount stopped",
                            "The background worker exited cleanly.",
                        );
                    }
                } else if self.background_stop_requested {
                    // The user terminated the worker; the kill is not a failure.
                } else if current != MountState::Failed {
                    set_status(
                        &self.status,
                        MountState::Failed,
                        "Background mount exited",
                        format!("Worker exited with {exit_status}."),
                    );
                }
                self.background_stop_requested = false;
            }
            Ok(Some(None)) | Ok(None) => {}
            Err(e) => {
                self.background_child = None;
                self.active_background = false;
                set_status(
                    &self.status,
                    MountState::Failed,
                    "Could not inspect background mount",
                    e.to_string(),
                );
            }
        }
    }

    fn collect_mount_thread(&mut self) {
        let finished = self.mount_thread.as_ref().is_some_and(JoinHandle::is_finished);
        if !finished {
            return;
        }

        if let Some(handle) = self.mount_thread.take()
            && handle.join().is_err()
        {
            set_status(
                &self.status,
                MountState::Failed,
                "Mount thread panicked",
                "The backend thread exited unexpectedly.",
            );
        }
        self.mount_shutdown = None;
        if !self.active_background {
            self.active_mount_point = None;
            self.active_source_label = None;
        }

        let current = self.live_state();
        if !matches!(current, MountState::Failed | MountState::Stopped) {
            set_status(
                &self.status,
                MountState::Stopped,
                "Unmounted",
                "The mount process has stopped.",
            );
        }
    }

    fn collect_stop_thread(&mut self) {
        if self.stop_thread.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = self.stop_thread.take()
        {
            let _ = handle.join();
        }
    }

    fn track_mounted_since(&mut self) {
        let mounted = self.live_state() == MountState::Mounted;
        match (mounted, self.mounted_since) {
            (true, None) => self.mounted_since = Some(Instant::now()),
            (false, Some(_)) => self.mounted_since = None,
            _ => {}
        }
    }

    // ── Frame layout ──────────────────────────────────────────────────

    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);

        // Identity block.
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("hf").size(17.0).strong().color(accent()));
                    ui.add_space(-4.0);
                    ui.label(RichText::new("mount").size(17.0).strong().color(text_primary()));
                });
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(10.5)
                        .color(muted_text()),
                );
            });
        });
        ui.add_space(18.0);

        // Navigation.
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let margin = egui::Margin::symmetric(8, 0);
            egui::Frame::new().inner_margin(margin).show(ui, |ui| {
                for (tab, label) in [
                    (Tab::Mount, "Mount"),
                    (Tab::Activity, "Activity"),
                    (Tab::Setup, "Setup"),
                ] {
                    if nav_item(ui, label, self.tab == tab) {
                        self.tab = tab;
                    }
                }
            });
        });

        // Live status pinned to the bottom of the sidebar.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(14.0);
            let status = &self.status_cache;
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(14, 0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("{} · NFS", platform::platform_label()))
                            .size(10.5)
                            .color(muted_text()),
                    );
                    if let Some(since) = self.mounted_since {
                        ui.label(
                            RichText::new(format!("Up {}", format_elapsed(since.elapsed().as_secs())))
                                .size(10.5)
                                .color(text_secondary()),
                        );
                    }
                    ui.add_space(4.0);
                    status_chip(ui, &status.state);
                });
        });
    }

    fn draw_status_bar(&mut self, ui: &mut egui::Ui) {
        let status = &self.status_cache;
        let rect = ui.max_rect();
        ui.painter()
            .hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0_f32, border()));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            status_chip(ui, &status.state);
            ui.label(
                RichText::new(&status.headline)
                    .size(12.0)
                    .strong()
                    .color(text_primary()),
            );
            ui.add(egui::Label::new(RichText::new(&status.detail).size(12.0).color(text_secondary())).truncate());
        });
        ui.add_space(8.0);
    }
}

impl eframe::App for MountGuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.collect_finished();
        self.refresh_status_cache();
        // Steady repaint for elapsed time and thread collection; worker
        // updates additionally wake the UI through the poller's callback.
        ui.ctx().request_repaint_after(Duration::from_millis(1000));

        egui::Panel::left("sidebar")
            .frame(egui::Frame::new().fill(sidebar_bg()))
            .exact_size(168.0)
            .resizable(false)
            .show_separator_line(true)
            .show(ui, |ui| self.draw_sidebar(ui));

        egui::Panel::bottom("status-bar")
            .frame(egui::Frame::new().fill(sidebar_bg()))
            .show_separator_line(false)
            .show(ui, |ui| self.draw_status_bar(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(app_bg())
                    .inner_margin(egui::Margin::symmetric(24, 20)),
            )
            .show(ui, |ui| match self.tab {
                Tab::Mount => self.draw_mount_tab(ui),
                Tab::Activity => self.draw_activity_tab(ui),
                Tab::Setup => self.draw_setup_tab(ui),
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.save_profile();

        // If the user just pressed Stop, `stop_mount` offloaded the unmount (or
        // post-termination cleanup) to a worker thread; let it finish (bounded)
        // so closing the window doesn't abort it and leave a mount behind. Only
        // reap if it actually finished — a wedged unmount must not hang close.
        if let Some(handle) = self.stop_thread.take() {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }

        // Background mounts survive the window by design.
        if self.active_background {
            return;
        }
        if let Some(shutdown) = &self.mount_shutdown {
            shutdown.request();
        }
        if let Some(handle) = &self.mount_thread {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            if !handle.is_finished()
                && let Some(mount_point) = self.active_mount_point.clone()
            {
                // The backend did not wind down in time — force the unmount so
                // no dead mount point is left behind. Run it on a detached thread
                // with a bounded wait: `unmount_path` can block on a wedged NFS
                // mount, and window close must stay bounded. If it doesn't finish
                // in time we drop the handle and let the exiting process reap it.
                let unmount = thread::spawn(move || {
                    let _ = platform::unmount_path(&mount_point);
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                while !unmount.is_finished() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

fn worker_mount_point(status: &WorkerStatus) -> Option<PathBuf> {
    status
        .mount_point
        .as_deref()
        .map(str::trim)
        .filter(|mount_point| !mount_point.is_empty())
        .map(PathBuf::from)
}

fn worker_mount_state(status: &WorkerStatus) -> MountState {
    match status.state {
        crate::worker::WorkerState::Mounting => MountState::Mounting,
        crate::worker::WorkerState::Mounted => MountState::Mounted,
        crate::worker::WorkerState::Stopping => MountState::Stopping,
        crate::worker::WorkerState::Stopped => MountState::Stopped,
        crate::worker::WorkerState::Failed => MountState::Failed,
    }
}

/// Foreground mount body: build the VFS, run the NFS backend, surface every
/// state change through `shared_status`. Runs on a dedicated thread.
fn run_mount(source: Source, options: MountOptions, shared_status: SharedMountStatus, shutdown: MountShutdown) {
    let status_for_events = shared_status.clone();
    let shutdown_probe = shutdown.clone();
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
            shutdown: Some(shutdown),
        };
        setup
            .runtime
            .block_on(hf_mount::nfs::mount_nfs_with_callback(
                virtual_fs,
                &mount_point,
                params,
                None,
                move |event| handle_mount_event(&status_for_events, event),
            ))
            .map_err(|e| e.to_string())
    }));

    match result {
        Ok(Ok(())) => set_status(
            &shared_status,
            MountState::Stopped,
            "Unmounted",
            "The mount stopped cleanly.",
        ),
        // An error after the user asked to stop is a consequence of the
        // teardown, not a failure worth alarming about.
        Ok(Err(message)) if shutdown_probe.is_requested() => set_status(
            &shared_status,
            MountState::Stopped,
            "Stopped",
            format!("Mount cancelled during startup ({message})."),
        ),
        Ok(Err(message)) => set_status(&shared_status, MountState::Failed, "Mount failed", message),
        Err(payload) => set_status(
            &shared_status,
            MountState::Failed,
            "Mount crashed",
            panic_message(payload),
        ),
    }
}

fn handle_mount_event(status: &SharedMountStatus, event: NfsMountEvent) {
    match event {
        NfsMountEvent::ServerListening { port } => set_status(
            status,
            MountState::Mounting,
            "Local NFS server is listening",
            format!("127.0.0.1:{port}"),
        ),
        NfsMountEvent::MountCommand { command } => push_log(status, format!("Running {command}")),
        NfsMountEvent::Mounted { mount_point } => set_status(
            status,
            MountState::Mounted,
            "Mounted",
            format!("Mounted at {mount_point}"),
        ),
        NfsMountEvent::ShuttingDown { reason } => set_status(status, MountState::Stopping, "Shutting down", reason),
    }
}
