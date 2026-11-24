//! The mount registry at the heart of a mount host (the background
//! supervisor or a foreground `hf-mount start`). It owns the mounts, enforces
//! one mount per target, tracks state truthfully and records activity.
//!
//! The registry is OS-independent: the actual volume creation is behind
//! [`Mounter`], so the state machine is unit-tested with a fake mounter.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use super::protocol::{
    ActivityEvent, Credential, ErrorCode, ErrorInfo, EventLevel, HostInfo, MountInfo, MountState, MountStats,
    PROTOCOL_VERSION, Request, Response, UnmountReport, now_rfc3339,
};
use super::spec::{MountSpec, normalize_target};

/// A live mount created by a [`Mounter`].
pub trait MountHandle: Send + Sync {
    /// Source as resolved by the Hub, e.g. `model/openai-community/gpt2/main`.
    fn resolved_source(&self) -> Option<String>;
    fn stats(&self) -> MountStats;
    /// `Some(reason)` when the volume stopped being served on its own.
    fn lost(&self) -> Option<String>;
    /// Wake the host when the volume stops serving. The host also checks
    /// once after registering, covering a loss during mount setup.
    fn notify_on_loss(&self, _wake: Arc<Notify>) {}
    /// Re-queue failed uploads; returns how many files were queued.
    fn retry_uploads(&self) -> usize;
    /// Remove the drive, drain pending uploads (bounded) and free resources.
    fn unmount(self: Box<Self>) -> UnmountReport;
}

/// Creates mounts. Implemented with WinFsp on Windows.
pub trait Mounter: Send + Sync {
    fn mount(&self, spec: &MountSpec, credential: Option<&Credential>) -> Result<Box<dyn MountHandle>, ErrorInfo>;
    /// Whether `target` is free on this system (not used by anything else).
    fn target_available(&self, target: &str) -> bool;
}

struct Entry {
    info: MountInfo,
    handle: Option<Box<dyn MountHandle>>,
}

const MAX_ACTIVITY: usize = 500;
const MAX_FAILED_RECORDS: usize = 20;

pub struct Registry {
    mounter: Arc<dyn Mounter>,
    host: HostInfo,
    entries: Mutex<Vec<Entry>>,
    activity: Mutex<VecDeque<ActivityEvent>>,
    clients: Mutex<ClientActivity>,
    /// One maintenance loop per host. A stored permit covers changes that
    /// arrive between inspecting the registry and starting to wait.
    changed: Arc<Notify>,
    journal_retry: AtomicBool,
    shutdown: AtomicBool,
    journal: Option<PathBuf>,
    /// Last journal content written, so unchanged state is not rewritten on
    /// every status query.
    journal_written: Mutex<Option<Vec<u8>>>,
}

struct ClientActivity {
    last: Instant,
    working: usize,
}

/// Covers the whole operation, including recovery after a driver loss.
struct Working<'a>(&'a Registry);

impl Drop for Working<'_> {
    fn drop(&mut self) {
        let mut clients = lock(&self.0.clients);
        clients.last = Instant::now();
        clients.working -= 1;
        self.0.changed.notify_one();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize)]
struct Journal<'a> {
    pid: u32,
    started_at: &'a str,
    mounts: Vec<JournalMount>,
}

#[derive(Serialize, serde::Deserialize)]
pub struct JournalMount {
    pub target: String,
    pub source: String,
}

impl Registry {
    pub fn new(mounter: Arc<dyn Mounter>, host: HostInfo, journal: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            mounter,
            host,
            entries: Mutex::new(Vec::new()),
            activity: Mutex::new(VecDeque::new()),
            clients: Mutex::new(ClientActivity {
                last: Instant::now(),
                working: 0,
            }),
            changed: Arc::new(Notify::new()),
            journal_retry: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            journal,
            journal_written: Mutex::new(None),
        })
    }

    pub fn host(&self) -> &HostInfo {
        &self.host
    }

    pub fn record(&self, level: EventLevel, target: Option<&str>, message: impl Into<String>) {
        let message = super::redact::redact(&message.into());
        match level {
            EventLevel::Info => info!(target: "hf_mount::activity", "{}{message}", fmt_target(target)),
            EventLevel::Warning => warn!(target: "hf_mount::activity", "{}{message}", fmt_target(target)),
            EventLevel::Error => error!(target: "hf_mount::activity", "{}{message}", fmt_target(target)),
        }
        let mut activity = lock(&self.activity);
        if activity.len() >= MAX_ACTIVITY {
            activity.pop_front();
        }
        activity.push_back(ActivityEvent {
            time: now_rfc3339(),
            level,
            target: target.map(str::to_string),
            message,
        });
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.changed.notify_one();
    }

    pub async fn changed(&self) {
        if self.journal_retry.load(Ordering::Relaxed) {
            // Keep trying to persist crash recovery state even with no clients.
            let _ = tokio::time::timeout(Duration::from_secs(30), self.changed.notified()).await;
        } else {
            self.changed.notified().await;
        }
    }

    /// Wait for a mount, unmount or recovery already in progress to finish
    /// before tearing down the runtime that its I/O depends on.
    pub async fn wait_for_work(&self) {
        while lock(&self.clients).working != 0 {
            self.changed.notified().await;
        }
    }

    /// While empty, sleep to the actual idle deadline. Claim shutdown under
    /// the same lock that admits work so a racing mount either wins or gets a
    /// shutdown response before it creates a volume.
    pub fn idle_shutdown_delay(&self, timeout: Duration) -> Option<Duration> {
        let clients = lock(&self.clients);
        if clients.working != 0 || self.active_count() != 0 {
            return None;
        }
        let remaining = timeout.saturating_sub(clients.last.elapsed());
        if remaining.is_zero() {
            self.shutdown.store(true, Ordering::SeqCst);
        }
        Some(remaining)
    }

    /// Time since the last operation that changed host state.
    pub fn idle_for(&self) -> Duration {
        lock(&self.clients).last.elapsed()
    }

    /// Number of mounts that are starting, mounted or stopping.
    pub fn active_count(&self) -> usize {
        lock(&self.entries)
            .iter()
            .filter(|e| e.info.state != MountState::Failed)
            .count()
    }

    /// Handle one client request. Blocking: call from a blocking thread.
    pub fn handle(&self, request: Request) -> Response {
        let _working = match &request {
            Request::Mount { .. }
            | Request::Unmount { .. }
            | Request::RetryUploads { .. }
            | Request::Dismiss { .. } => {
                let mut clients = lock(&self.clients);
                if self.shutdown_requested() {
                    return error_response(
                        ErrorInfo::new(ErrorCode::HostUnavailable, "this hf-mount host is shutting down")
                            .with_hint("try again in a moment"),
                    );
                }
                clients.working += 1;
                Some(Working(self))
            }
            Request::Hello { .. } | Request::List | Request::Activity { .. } | Request::Shutdown => None,
        };
        match request {
            Request::Hello { protocol, .. } => {
                if protocol != PROTOCOL_VERSION {
                    return error_response(
                        ErrorInfo::new(
                            ErrorCode::ProtocolMismatch,
                            format!("client protocol {protocol} does not match host protocol {PROTOCOL_VERSION}"),
                        )
                        .with_hint("stop the running hf-mount supervisor (hf-mount stop --all) and retry"),
                    );
                }
                Response::Hello {
                    host: self.host.clone(),
                }
            }
            Request::List => Response::Mounts {
                host: self.host.clone(),
                mounts: self.list(),
            },
            Request::Mount { spec, credential } => match self.mount(*spec, credential) {
                Ok(mount) => Response::Mounted { mount: Box::new(mount) },
                Err(e) => error_response(e),
            },
            Request::Unmount { target, force } => match self.unmount(&target, force) {
                Ok(report) => Response::Unmounted { report },
                Err(e) => error_response(e),
            },
            Request::Dismiss { target } => match self.dismiss(&target) {
                Ok(()) => Response::Ok,
                Err(e) => error_response(e),
            },
            Request::RetryUploads { target } => match self.retry_uploads(&target) {
                Ok(_) => Response::Ok,
                Err(e) => error_response(e),
            },
            Request::Activity { limit } => {
                let activity = lock(&self.activity);
                let skip = activity.len().saturating_sub(limit);
                Response::Activity {
                    events: activity.iter().skip(skip).cloned().collect(),
                }
            }
            Request::Shutdown => {
                self.request_shutdown();
                Response::Ok
            }
        }
    }

    /// Mark mounts whose volume disappeared as failed (and release them).
    pub fn check_health(&self) {
        let mut lost = Vec::new();
        let _working = {
            let mut clients = lock(&self.clients);
            if self.shutdown_requested() {
                return;
            }
            let mut entries = lock(&self.entries);
            for entry in entries.iter_mut() {
                if entry.info.state != MountState::Mounted {
                    continue;
                }
                if let Some(reason) = entry.handle.as_ref().and_then(|h| h.lost()) {
                    entry.info.state = MountState::Stopping;
                    entry.info.error = Some(ErrorInfo::new(ErrorCode::DriverUnavailable, reason.clone()));
                    entry.info.stats = None;
                    if let Some(handle) = entry.handle.take() {
                        lost.push((entry.info.spec.target.clone(), reason, handle));
                    }
                }
            }
            if lost.is_empty() {
                None
            } else {
                clients.working += 1;
                Some(Working(self))
            }
        };
        if lost.is_empty() {
            if self.journal_retry.load(Ordering::Relaxed) {
                self.write_journal();
            }
            return;
        }
        // A lost volume may still have uploads to drain or files to recover.
        // Keep it active and reserve its letter until the handle is released.
        for (target, reason, handle) in lost {
            self.record(EventLevel::Error, Some(&target), format!("Mount lost: {reason}"));
            let report = handle.unmount();
            if let Some(summary) = report.incomplete_summary() {
                self.record(EventLevel::Error, Some(&target), summary);
            }
            for w in report.warnings {
                self.record(EventLevel::Warning, Some(&target), w);
            }
            if let Some(entry) = lock(&self.entries).iter_mut().find(|e| e.info.spec.target == target) {
                entry.info.state = MountState::Failed;
            }
        }
        self.write_journal();
    }

    pub fn list(&self) -> Vec<MountInfo> {
        lock(&self.entries)
            .iter()
            .map(|e| {
                let mut info = e.info.clone();
                if let Some(handle) = &e.handle {
                    if let Some(reason) = handle.lost() {
                        // The monitor drains and recovers outside the IPC request.
                        // Report the loss even if this read beat the notification.
                        info.state = MountState::Stopping;
                        info.error = Some(ErrorInfo::new(ErrorCode::DriverUnavailable, reason));
                        info.stats = None;
                        self.changed.notify_one();
                    } else {
                        info.stats = Some(handle.stats());
                    }
                }
                info
            })
            .collect()
    }

    fn mount(&self, spec: MountSpec, credential: Option<Credential>) -> Result<MountInfo, ErrorInfo> {
        let spec = spec
            .validated()
            .map_err(|e| ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string()))?;
        let target = spec.target.clone();
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut entries = lock(&self.entries);
            // Checked under the entries lock, which `unmount_all` takes to
            // list what to stop, so no mount slips in behind the teardown.
            if self.shutdown_requested() {
                return Err(
                    ErrorInfo::new(ErrorCode::HostUnavailable, "this hf-mount host is shutting down")
                        .with_hint("try again in a moment"),
                );
            }
            if let Some(existing) = entries
                .iter()
                .find(|e| e.info.spec.target == target && e.info.state != MountState::Failed)
            {
                return Err(ErrorInfo::new(
                    ErrorCode::AlreadyMounted,
                    format!(
                        "{target} is already used by hf-mount ({})",
                        existing.info.spec.display_source()
                    ),
                )
                .with_hint(format!("stop it first with: hf-mount stop {target}")));
            }
            if !self.mounter.target_available(&target) {
                return Err(
                    ErrorInfo::new(ErrorCode::TargetInUse, format!("drive {target} is already in use"))
                        .with_hint("choose another letter (hf-mount drives lists free letters)"),
                );
            }
            entries.retain(|e| e.info.spec.target != target);
            entries.push(Entry {
                info: MountInfo {
                    id: id.clone(),
                    spec: spec.clone(),
                    state: MountState::Starting,
                    resolved_source: None,
                    credential_source: credential.as_ref().map(Credential::source_label),
                    host_pid: self.host.pid,
                    host_kind: self.host.kind,
                    requested_at: now_rfc3339(),
                    mounted_at: None,
                    error: None,
                    stats: None,
                },
                handle: None,
            });
        }
        self.record(
            EventLevel::Info,
            Some(&target),
            format!(
                "Mounting {} ({})",
                spec.display_source(),
                if spec.read_only { "read-only" } else { "read-write" }
            ),
        );

        let result = self.mounter.mount(&spec, credential.as_ref());

        let mut orphan = None;
        let outcome = {
            let mut entries = lock(&self.entries);
            match entries.iter_mut().find(|e| e.info.id == id) {
                None => {
                    // Removed while mounting (shutdown): release the volume.
                    orphan = result.ok();
                    Err(ErrorInfo::new(ErrorCode::Internal, "mount was cancelled"))
                }
                Some(entry) => match result {
                    Ok(handle) => {
                        entry.info.state = MountState::Mounted;
                        entry.info.mounted_at = Some(now_rfc3339());
                        entry.info.resolved_source = handle.resolved_source();
                        let mut info = entry.info.clone();
                        info.stats = Some(handle.stats());
                        handle.notify_on_loss(self.changed.clone());
                        entry.handle = Some(handle);
                        Ok(info)
                    }
                    Err(e) => {
                        entry.info.state = MountState::Failed;
                        entry.info.error = Some(e.clone());
                        Err(e)
                    }
                },
            }
        };
        if let Some(handle) = orphan {
            handle.unmount();
        }
        match &outcome {
            Ok(info) => self.record(
                EventLevel::Info,
                Some(&target),
                format!(
                    "Mounted {} at {target}",
                    info.resolved_source.clone().unwrap_or_else(|| spec.display_source())
                ),
            ),
            Err(e) => self.record(EventLevel::Error, Some(&target), format!("Mount failed: {e}")),
        }
        self.prune_failed();
        self.write_journal();
        outcome
    }

    fn unmount(&self, target: &str, force: bool) -> Result<UnmountReport, ErrorInfo> {
        let target = normalize_target(target).map_err(|e| ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string()))?;
        let handle = {
            let mut entries = lock(&self.entries);
            let Some(pos) = entries.iter().position(|e| e.info.spec.target == target) else {
                return Err(ErrorInfo::new(
                    ErrorCode::NotMounted,
                    format!("nothing is mounted at {target} by hf-mount"),
                ));
            };
            match entries[pos].info.state {
                MountState::Failed => {
                    entries.remove(pos);
                    return Ok(UnmountReport {
                        target,
                        warnings: vec!["the mount had already failed; its record was cleared".to_string()],
                        ..Default::default()
                    });
                }
                MountState::Starting | MountState::Stopping => {
                    return Err(ErrorInfo::new(
                        ErrorCode::AlreadyMounted,
                        format!(
                            "{target} is still {}; try again in a moment",
                            state_word(entries[pos].info.state)
                        ),
                    ));
                }
                MountState::Mounted => {}
            }
            let entry = &mut entries[pos];
            if let Some(handle) = &entry.handle {
                let stats = handle.stats();
                if stats.open_write_handles > 0 && !force {
                    return Err(ErrorInfo::new(
                        ErrorCode::BusyWithOpenFiles,
                        format!(
                            "{} file(s) on {target} are open for writing; unmounting now could lose unsaved data",
                            stats.open_write_handles
                        ),
                    )
                    .with_hint("close the applications using them, or force with --force"));
                }
            }
            entry.info.state = MountState::Stopping;
            entry.handle.take()
        };
        self.record(EventLevel::Info, Some(&target), "Unmounting");
        let report = match handle {
            Some(handle) => handle.unmount(),
            None => UnmountReport {
                target: target.clone(),
                ..Default::default()
            },
        };
        lock(&self.entries).retain(|e| e.info.spec.target != target);
        if report.unflushed_files > 0 {
            self.record(
                EventLevel::Error,
                Some(&target),
                format!(
                    "{} file(s) were not uploaded before unmount{}",
                    report.unflushed_files,
                    match (&report.recovered_to, &report.kept_at) {
                        (Some(dir), Some(kept)) => format!(
                            "; copies saved to {}, the others' data kept in {}",
                            dir.display(),
                            kept.display()
                        ),
                        (None, Some(kept)) => format!("; their data was kept in {}", kept.display()),
                        (Some(dir), None) => format!("; copies saved to {}", dir.display()),
                        (None, None) => String::new(),
                    }
                ),
            );
        }
        if report.unfinished_deletes > 0 {
            self.record(
                EventLevel::Error,
                Some(&target),
                format!(
                    "{} deletion(s) were not committed before unmount; those files are still on the Hub",
                    report.unfinished_deletes
                ),
            );
        }
        if report.complete() {
            self.record(EventLevel::Info, Some(&target), "Unmounted");
        }
        for w in &report.warnings {
            self.record(EventLevel::Warning, Some(&target), w.clone());
        }
        self.write_journal();
        Ok(report)
    }

    fn dismiss(&self, target: &str) -> Result<(), ErrorInfo> {
        let target = normalize_target(target).map_err(|e| ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string()))?;
        let mut entries = lock(&self.entries);
        let before = entries.len();
        entries.retain(|e| !(e.info.spec.target == target && e.info.state == MountState::Failed));
        if entries.len() == before {
            return Err(ErrorInfo::new(
                ErrorCode::NotMounted,
                format!("no failed mount recorded for {target}"),
            ));
        }
        Ok(())
    }

    fn retry_uploads(&self, target: &str) -> Result<usize, ErrorInfo> {
        let target = normalize_target(target).map_err(|e| ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string()))?;
        let entries = lock(&self.entries);
        let handle = entries
            .iter()
            .find(|e| e.info.spec.target == target)
            .and_then(|e| e.handle.as_ref())
            .ok_or_else(|| ErrorInfo::new(ErrorCode::NotMounted, format!("nothing is mounted at {target}")))?;
        Ok(handle.retry_uploads())
    }

    /// Unmount everything (host exit). Open writers do not block this.
    pub fn unmount_all(&self) -> Vec<UnmountReport> {
        let targets: Vec<String> = lock(&self.entries)
            .iter()
            .filter(|e| e.info.state == MountState::Mounted)
            .map(|e| e.info.spec.target.clone())
            .collect();
        let mut reports = Vec::new();
        for target in targets {
            match self.unmount(&target, true) {
                Ok(report) => reports.push(report),
                Err(e) => warn!("unmount {target} during shutdown failed: {e}"),
            }
        }
        lock(&self.entries).clear();
        self.remove_journal();
        reports
    }

    fn prune_failed(&self) {
        let mut entries = lock(&self.entries);
        let failed = entries.iter().filter(|e| e.info.state == MountState::Failed).count();
        let mut excess = failed.saturating_sub(MAX_FAILED_RECORDS);
        entries.retain(|e| {
            if excess > 0 && e.info.state == MountState::Failed {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }

    fn write_journal(&self) {
        let Some(path) = &self.journal else { return };
        // Serialize writers before taking the snapshot so an older snapshot
        // cannot overwrite a newer one after a concurrent lifecycle change.
        let mut written = lock(&self.journal_written);
        let mounts: Vec<JournalMount> = lock(&self.entries)
            .iter()
            .filter(|e| e.info.state == MountState::Mounted)
            .map(|e| JournalMount {
                target: e.info.spec.target.clone(),
                source: e.info.spec.display_source(),
            })
            .collect();
        let journal = Journal {
            pid: self.host.pid,
            started_at: &self.host.started_at,
            mounts,
        };
        let Ok(json) = serde_json::to_vec_pretty(&journal) else {
            return;
        };
        if written.as_deref() == Some(json.as_slice()) {
            self.journal_retry.store(false, Ordering::Relaxed);
            return;
        }
        // Write a temporary file and rename it over the journal, so a crash
        // mid-write never leaves a torn journal behind.
        let tmp = path.with_extension("json.tmp");
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&tmp, &json))
            .and_then(|()| std::fs::rename(&tmp, path));
        self.journal_retry.store(result.is_err(), Ordering::Relaxed);
        match result {
            Ok(()) => *written = Some(json),
            Err(e) => warn!("could not write session journal {}: {e}", path.display()),
        }
    }

    pub fn remove_journal(&self) {
        if let Some(path) = &self.journal {
            let mut written = lock(&self.journal_written);
            let _ = std::fs::remove_file(path);
            *written = None;
            self.journal_retry.store(false, Ordering::Relaxed);
        }
    }
}

fn state_word(state: MountState) -> &'static str {
    match state {
        MountState::Starting => "starting",
        MountState::Mounted => "mounted",
        MountState::Stopping => "stopping",
        MountState::Failed => "failed",
    }
}

fn fmt_target(target: Option<&str>) -> String {
    target.map(|t| format!("[{t}] ")).unwrap_or_default()
}

fn error_response(error: ErrorInfo) -> Response {
    Response::Error { error }
}

/// Contents of a session journal left by a supervisor that exited.
#[derive(serde::Deserialize)]
pub struct JournalFile {
    pub pid: u32,
    pub started_at: String,
    pub mounts: Vec<JournalMount>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::protocol::HostKind;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct FakeState {
        taken: Mutex<Vec<String>>,
        fail_with: Mutex<Option<ErrorInfo>>,
        unmounted: AtomicUsize,
        teardown: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    }

    struct FakeMounter(Arc<FakeState>);

    struct FakeHandle {
        state: Arc<FakeState>,
        target: String,
        open_writers: usize,
        unflushed: usize,
        lost: Arc<AtomicBool>,
    }

    impl MountHandle for FakeHandle {
        fn resolved_source(&self) -> Option<String> {
            Some("model/acme/thing/main".into())
        }
        fn stats(&self) -> MountStats {
            MountStats {
                open_write_handles: self.open_writers,
                pending_uploads: self.unflushed,
                ..Default::default()
            }
        }
        fn lost(&self) -> Option<String> {
            self.lost.load(Ordering::SeqCst).then(|| "driver unloaded".to_string())
        }
        fn retry_uploads(&self) -> usize {
            self.unflushed
        }
        fn unmount(self: Box<Self>) -> UnmountReport {
            if let Some((started, finish)) = lock(&self.state.teardown).take() {
                started.send(()).unwrap();
                finish.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            self.state.unmounted.fetch_add(1, Ordering::SeqCst);
            lock(&self.state.taken).retain(|t| t != &self.target);
            UnmountReport {
                target: self.target.clone(),
                unflushed_files: self.unflushed,
                recovered_to: (self.unflushed > 0).then(|| PathBuf::from("/recovered")),
                ..Default::default()
            }
        }
    }

    thread_local! {
        static NEXT: std::cell::RefCell<(usize, usize, Option<Arc<AtomicBool>>)> = const { std::cell::RefCell::new((0, 0, None)) };
    }

    impl Mounter for FakeMounter {
        fn mount(&self, spec: &MountSpec, _c: Option<&Credential>) -> Result<Box<dyn MountHandle>, ErrorInfo> {
            if let Some(e) = lock(&self.0.fail_with).take() {
                return Err(e);
            }
            lock(&self.0.taken).push(spec.target.clone());
            let (open_writers, unflushed, lost) = NEXT.with(|n| n.borrow_mut().clone());
            Ok(Box::new(FakeHandle {
                state: self.0.clone(),
                target: spec.target.clone(),
                open_writers,
                unflushed,
                lost: lost.unwrap_or_default(),
            }))
        }
        fn target_available(&self, target: &str) -> bool {
            target != "Z:" && !lock(&self.0.taken).iter().any(|t| t == target)
        }
    }

    fn registry() -> (Arc<Registry>, Arc<FakeState>) {
        let state = Arc::new(FakeState::default());
        let host = HostInfo {
            pid: 42,
            kind: HostKind::Supervisor,
            version: "test".into(),
            protocol: PROTOCOL_VERSION,
            started_at: now_rfc3339(),
            elevated: false,
            winfsp_version: None,
        };
        (Registry::new(Arc::new(FakeMounter(state.clone())), host, None), state)
    }

    fn mount_req(id: &str, target: &str) -> Request {
        Request::Mount {
            spec: Box::new(MountSpec::parse("repo", id, target).unwrap()),
            credential: None,
        }
    }

    fn mounts(r: &Registry) -> Vec<MountInfo> {
        match r.handle(Request::List) {
            Response::Mounts { mounts, .. } => mounts,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mount_list_unmount_lifecycle() {
        let (r, state) = registry();
        let Response::Mounted { mount } = r.handle(mount_req("acme/thing", "H:")) else {
            panic!("mount failed")
        };
        assert_eq!(mount.state, MountState::Mounted);
        assert_eq!(mount.resolved_source.as_deref(), Some("model/acme/thing/main"));
        assert_eq!(mounts(&r).len(), 1);
        assert_eq!(r.active_count(), 1);
        let Response::Unmounted { report } = r.handle(Request::Unmount {
            target: "h".into(),
            force: false,
        }) else {
            panic!("unmount failed")
        };
        assert_eq!(report.target, "H:");
        assert_eq!(state.unmounted.load(Ordering::SeqCst), 1);
        assert!(mounts(&r).is_empty());
    }

    #[test]
    fn duplicate_and_occupied_targets_are_refused() {
        let (r, _) = registry();
        assert!(matches!(r.handle(mount_req("acme/a", "H:")), Response::Mounted { .. }));
        match r.handle(mount_req("acme/b", "H:")) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::AlreadyMounted),
            other => panic!("{other:?}"),
        }
        match r.handle(mount_req("acme/b", "Z:")) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::TargetInUse),
            other => panic!("{other:?}"),
        }
        assert_eq!(mounts(&r).len(), 1, "refused requests leave no record");
    }

    #[test]
    fn failures_are_recorded_then_cleared() {
        let (r, state) = registry();
        *lock(&state.fail_with) = Some(ErrorInfo::new(ErrorCode::NotFound, "nope"));
        match r.handle(mount_req("acme/missing", "K:")) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::NotFound),
            other => panic!("{other:?}"),
        }
        let listed = mounts(&r);
        assert_eq!(listed[0].state, MountState::Failed);
        assert_eq!(listed[0].error.as_ref().unwrap().code, ErrorCode::NotFound);
        assert_eq!(r.active_count(), 0);
        // A new mount on the same letter replaces the failed record.
        assert!(matches!(r.handle(mount_req("acme/ok", "K:")), Response::Mounted { .. }));
        assert_eq!(mounts(&r).len(), 1);
        // Dismiss only applies to failed records.
        assert!(matches!(
            r.handle(Request::Dismiss { target: "K:".into() }),
            Response::Error { .. }
        ));
    }

    #[test]
    fn open_writers_block_unmount_unless_forced() {
        let (r, state) = registry();
        NEXT.with(|n| *n.borrow_mut() = (2, 0, None));
        assert!(matches!(r.handle(mount_req("acme/w", "W:")), Response::Mounted { .. }));
        NEXT.with(|n| *n.borrow_mut() = (0, 0, None));
        match r.handle(Request::Unmount {
            target: "W:".into(),
            force: false,
        }) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::BusyWithOpenFiles),
            other => panic!("{other:?}"),
        }
        assert_eq!(mounts(&r)[0].state, MountState::Mounted, "still mounted after refusal");
        assert!(matches!(
            r.handle(Request::Unmount {
                target: "W:".into(),
                force: true
            }),
            Response::Unmounted { .. }
        ));
        assert_eq!(state.unmounted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unflushed_uploads_are_reported() {
        let (r, _) = registry();
        NEXT.with(|n| *n.borrow_mut() = (0, 3, None));
        assert!(matches!(r.handle(mount_req("acme/u", "U:")), Response::Mounted { .. }));
        NEXT.with(|n| *n.borrow_mut() = (0, 0, None));
        let Response::Unmounted { report } = r.handle(Request::Unmount {
            target: "U:".into(),
            force: false,
        }) else {
            panic!()
        };
        assert_eq!(report.unflushed_files, 3);
        assert!(report.recovered_to.is_some());
        let Response::Activity { events } = r.handle(Request::Activity { limit: 50 }) else {
            panic!()
        };
        assert!(
            events
                .iter()
                .any(|e| e.level == EventLevel::Error && e.message.contains("not uploaded"))
        );
    }

    #[test]
    fn lost_volumes_become_failed() {
        let (r, state) = registry();
        let lost = Arc::new(AtomicBool::new(false));
        NEXT.with(|n| *n.borrow_mut() = (0, 0, Some(lost.clone())));
        assert!(matches!(r.handle(mount_req("acme/l", "L:")), Response::Mounted { .. }));
        NEXT.with(|n| *n.borrow_mut() = (0, 0, None));
        lost.store(true, Ordering::SeqCst);
        let pending = mounts(&r);
        assert_eq!(pending[0].state, MountState::Stopping);
        assert_eq!(
            state.unmounted.load(Ordering::SeqCst),
            0,
            "a status read must not drain uploads"
        );
        r.check_health();
        let listed = mounts(&r);
        assert_eq!(listed[0].state, MountState::Failed);
        assert_eq!(listed[0].error.as_ref().unwrap().code, ErrorCode::DriverUnavailable);
        assert_eq!(
            state.unmounted.load(Ordering::SeqCst),
            1,
            "lost volume resources released"
        );
    }

    #[test]
    fn protocol_mismatch_and_not_mounted() {
        let (r, _) = registry();
        match r.handle(Request::Hello {
            protocol: 999,
            client: "x".into(),
        }) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::ProtocolMismatch),
            other => panic!("{other:?}"),
        }
        match r.handle(Request::Unmount {
            target: "Q:".into(),
            force: false,
        }) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::NotMounted),
            other => panic!("{other:?}"),
        }
        assert!(matches!(r.handle(Request::Shutdown), Response::Ok));
        assert!(r.shutdown_requested());
    }

    #[test]
    fn passive_clients_do_not_extend_idle_lifetime() {
        let (r, _) = registry();
        let last = lock(&r.clients).last;
        for _ in 0..10 {
            r.handle(Request::Hello {
                protocol: PROTOCOL_VERSION,
                client: "desktop".into(),
            });
            r.handle(Request::List);
            r.handle(Request::Activity { limit: 300 });
        }
        assert_eq!(lock(&r.clients).last, last);
        assert_eq!(r.idle_shutdown_delay(Duration::ZERO), Some(Duration::ZERO));
        assert!(r.shutdown_requested());
        assert!(matches!(r.handle(mount_req("acme/late", "H:")), Response::Error { .. }));
    }

    #[test]
    fn idle_deadline_starts_after_last_operation_finishes() {
        let (r, _) = registry();
        let timeout = Duration::from_secs(600);
        r.handle(mount_req("acme/active", "H:"));
        assert_eq!(r.idle_shutdown_delay(Duration::ZERO), None);
        let mounted = lock(&r.clients).last;
        // Ensure an implementation that leaves the timestamp at mount time
        // cannot pass even on a clock with coarse resolution.
        std::thread::sleep(Duration::from_millis(20));
        assert!(matches!(
            r.handle(Request::Unmount {
                target: "H:".into(),
                force: false
            }),
            Response::Unmounted { .. }
        ));
        assert!(lock(&r.clients).last > mounted);
        assert!(r.idle_shutdown_delay(timeout).unwrap() > timeout / 2);
        assert!(!r.shutdown_requested());
    }

    #[test]
    fn lost_volume_recovery_reserves_the_letter_and_keeps_host_alive() {
        let (r, state) = registry();
        let lost = Arc::new(AtomicBool::new(false));
        NEXT.with(|n| *n.borrow_mut() = (0, 3, Some(lost.clone())));
        r.handle(mount_req("acme/recover", "H:"));
        NEXT.with(|n| *n.borrow_mut() = (0, 0, None));
        let (started, recovering) = std::sync::mpsc::channel();
        let (finish, resume) = std::sync::mpsc::channel();
        *lock(&state.teardown) = Some((started, resume));
        lost.store(true, Ordering::SeqCst);
        std::thread::scope(|scope| {
            scope.spawn(|| r.check_health());
            recovering.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(mounts(&r)[0].state, MountState::Stopping);
            assert_eq!(r.idle_shutdown_delay(Duration::ZERO), None);
            assert!(matches!(
                r.handle(mount_req("acme/other", "H:")),
                Response::Error { .. }
            ));
            finish.send(()).unwrap();
        });
        assert_eq!(mounts(&r)[0].state, MountState::Failed);
        assert_eq!(state.unmounted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_new_mounts_once_shutting_down() {
        let (r, state) = registry();
        assert!(matches!(r.handle(Request::Shutdown), Response::Ok));
        match r.handle(mount_req("acme/late", "L:")) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::HostUnavailable),
            other => panic!("{other:?}"),
        }
        assert!(lock(&state.taken).is_empty(), "nothing was mounted");
        assert!(mounts(&r).is_empty());
    }

    #[test]
    fn unmount_all_clears_everything() {
        let (r, state) = registry();
        for (id, t) in [("acme/a", "D:"), ("acme/b", "E:")] {
            assert!(matches!(r.handle(mount_req(id, t)), Response::Mounted { .. }));
        }
        assert_eq!(r.unmount_all().len(), 2);
        assert_eq!(state.unmounted.load(Ordering::SeqCst), 2);
        assert!(mounts(&r).is_empty());
    }
}
