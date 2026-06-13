//! Application state and frame layout: header with tabs, central tab body,
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
use crate::widgets::{status_chip, tab_button};
use crate::worker::{WorkerPoller, WorkerSnapshot, WorkerStatus, spawn_background_worker};

const MAX_LOG_LINES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Mount,
    Activity,
    Setup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountState {
    Ready,
    Mounting,
    Mounted,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug)]
pub struct SharedStatus {
    pub state: MountState,
    pub headline: String,
    pub detail: String,
    pub log: VecDeque<String>,
}

impl Default for SharedStatus {
    fn default() -> Self {
        let mut log = VecDeque::new();
        log.push_back("Ready".to_string());
        Self {
            state: MountState::Ready,
            headline: "Ready".to_string(),
            detail: "Configure a source and start the mount.".to_string(),
            log,
        }
    }
}

pub type SharedMountStatus = Arc<Mutex<SharedStatus>>;

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
    push_log_locked(&mut status, format!("{headline}: {detail}"));
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
    push_log_locked(&mut status, format!("{headline}: {detail}"));
}

pub fn push_log(status: &SharedMountStatus, message: impl Into<String>) {
    let mut status = status.lock().expect("status mutex poisoned");
    push_log_locked(&mut status, message.into());
}

fn push_log_locked(status: &mut SharedStatus, message: String) {
    status.log.push_back(message);
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
    pub autostart_enabled: bool,
    pub show_advanced: bool,
    pub recent_sources: Vec<RecentSource>,

    // UI state.
    pub tab: Tab,
    pub checks: Vec<CheckItem>,

    // Mount state.
    pub status: SharedMountStatus,
    mount_thread: Option<JoinHandle<()>>,
    mount_shutdown: Option<MountShutdown>,
    stop_thread: Option<JoinHandle<()>>,
    background_child: Option<Child>,
    pub active_background: bool,
    pub active_mount_point: Option<PathBuf>,
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
            autostart_enabled: crate::autostart::autostart_is_enabled(),
            show_advanced: false,
            recent_sources: Vec::new(),
            tab: Tab::Mount,
            checks,
            status: Arc::new(Mutex::new(SharedStatus::default())),
            mount_thread: None,
            mount_shutdown: None,
            stop_thread: None,
            background_child: None,
            active_background: false,
            active_mount_point: None,
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
            Err(e) => push_log(&app.status, format!("Could not load saved settings: {e}")),
        }

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

    pub fn current_status(&self) -> SharedStatus {
        self.status.lock().expect("status mutex poisoned").clone()
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
            match spawn_background_worker(&mount_point) {
                Ok(child) => {
                    self.background_child = Some(child);
                    self.active_background = true;
                    self.active_mount_point = Some(mount_point);
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
        // poll and the kill — clean up any mount it managed to create.
        if let Some(mount_point) = self.active_mount_point.clone() {
            thread::spawn(move || {
                let _ = platform::unmount_path(&mount_point);
            });
        }

        self.active_background = false;
        self.active_mount_point = None;
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
            push_log(&self.status, format!("Could not read background status: {error}"));
            return;
        }

        let Some(status) = snapshot.status else {
            self.last_worker_status = None;
            if self.active_background {
                self.active_background = false;
                self.active_mount_point = None;
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
                // The worker's own status file usually carries a more specific
                // message; only fall back to the exit code when it didn't.
                let current = self.current_status().state;
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
        }

        let current = self.current_status().state;
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
        let mounted = self.current_status().state == MountState::Mounted;
        match (mounted, self.mounted_since) {
            (true, None) => self.mounted_since = Some(Instant::now()),
            (false, Some(_)) => self.mounted_since = None,
            _ => {}
        }
    }

    // ── Frame layout ──────────────────────────────────────────────────

    fn draw_header(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(RichText::new("hf-mount").size(16.0).strong().color(text_primary()));
            ui.label(
                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .size(11.0)
                    .color(muted_text()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                let status = self.current_status();
                status_chip(ui, &status.state);
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.spacing_mut().item_spacing.x = 20.0;
            for (tab, label) in [
                (Tab::Mount, "Mount"),
                (Tab::Activity, "Activity"),
                (Tab::Setup, "Setup"),
            ] {
                if tab_button(ui, label, self.tab == tab) {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(8.0);
        let rect = ui.max_rect();
        ui.painter()
            .hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, border()));
    }

    fn draw_status_bar(&mut self, ui: &mut egui::Ui) {
        let status = self.current_status();
        let rect = ui.max_rect();
        ui.painter()
            .hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, border()));
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!("{} · NFS", platform::platform_label()))
                        .size(11.0)
                        .color(muted_text()),
                );
                if let Some(since) = self.mounted_since {
                    ui.label(
                        RichText::new(format_elapsed(since.elapsed().as_secs()))
                            .size(11.0)
                            .color(text_secondary()),
                    );
                }
            });
        });
        ui.add_space(8.0);
    }
}

impl eframe::App for MountGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.collect_finished();
        // Steady repaint for elapsed time and thread collection; worker
        // updates additionally wake the UI through the poller's callback.
        ctx.request_repaint_after(Duration::from_millis(1000));

        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::none().fill(header_bg()))
            .show_separator_line(false)
            .show(ctx, |ui| self.draw_header(ui));

        egui::TopBottomPanel::bottom("status-bar")
            .frame(egui::Frame::none().fill(header_bg()))
            .show_separator_line(false)
            .show(ctx, |ui| self.draw_status_bar(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(app_bg())
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0)),
            )
            .show(ctx, |ui| match self.tab {
                Tab::Mount => self.draw_mount_tab(ui),
                Tab::Activity => self.draw_activity_tab(ui),
                Tab::Setup => self.draw_setup_tab(ui),
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.save_profile();

        // Background mounts survive the window by design — but if the user
        // just pressed Stop, `stop_mount` offloaded the unmount to a worker
        // thread; let it finish (bounded) so closing the window doesn't abort
        // the stop and leave a live mount behind.
        if self.active_background {
            if let Some(handle) = self.stop_thread.take() {
                let deadline = Instant::now() + Duration::from_secs(8);
                while !handle.is_finished() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(50));
                }
                let _ = handle.join();
            }
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
                && let Some(mount_point) = &self.active_mount_point
            {
                // The backend did not wind down in time — force the unmount so
                // no dead mount point is left behind.
                let _ = platform::unmount_path(mount_point);
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
