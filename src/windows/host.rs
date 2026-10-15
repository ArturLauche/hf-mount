//! Mount hosts: the background supervisor (`hf-mount supervise`) and the
//! foreground host (`hf-mount start … --foreground`).

use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};

use super::client::journal_path;
use super::pipe::{self, Identity};
use super::{mounter, security};
use crate::manager::config::{self, logs_dir};
use crate::manager::logging::RedactingWriter;
use crate::manager::protocol::{
    Credential, EventLevel, HostInfo, HostKind, PROTOCOL_VERSION, Request, Response, UnmountReport, now_rfc3339,
};
use crate::manager::registry::Registry;
use crate::manager::spec::MountSpec;

/// The supervisor exits after this long with no mounts and no client requests.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

fn env_filter() -> tracing_subscriber::EnvFilter {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::EnvFilter::from_default_env()
    } else {
        tracing_subscriber::EnvFilter::new("hf_mount=info")
    }
}

/// Log to `%LOCALAPPDATA%\hf-mount\logs\<name>`, redacted.
pub fn init_file_logging(name: &str) -> std::path::PathBuf {
    let path = logs_dir().join(name);
    match RedactingWriter::file(&path) {
        Ok(writer) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_ansi(false)
                .with_writer(writer)
                .try_init();
        }
        Err(_) => init_stderr_logging(),
    }
    path
}

/// Log to stderr, redacted.
pub fn init_stderr_logging() {
    use std::io::IsTerminal;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(RedactingWriter::stderr())
        .try_init();
}

fn host_info(kind: HostKind) -> HostInfo {
    HostInfo {
        pid: std::process::id(),
        kind,
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: PROTOCOL_VERSION,
        started_at: now_rfc3339(),
        elevated: security::is_elevated(),
        winfsp_version: super::winfsp::api().ok().and_then(|a| a.version_string()),
    }
}

/// Run `f` on a plain OS thread and await it (keeps blocking engine work off
/// tokio workers).
async fn off_runtime<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.ok()
}

/// `hf-mount supervise`: host background mounts until idle or asked to stop.
pub fn run_supervisor() -> i32 {
    let log = init_file_logging("supervisor.log");
    crate::setup::apply_xet_env_defaults();
    let identity = match Identity::current() {
        Ok(id) => id,
        Err(e) => {
            error!("cannot determine the current user: {e}");
            return 1;
        }
    };
    let settings = config::load_settings().unwrap_or_else(|e| {
        warn!("settings unreadable, using defaults: {e}");
        config::Settings::default()
    });
    let runtime = crate::setup::build_runtime();
    let registry = Registry::new(
        mounter::mounter(runtime.handle().clone(), settings),
        host_info(HostKind::Supervisor),
        Some(journal_path(&identity)),
    );
    let name = identity.supervisor_pipe();
    let bound = {
        let _guard = runtime.enter();
        pipe::bind(&name, &identity.sid, true)
    };
    let (server, sd) = match bound {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            info!("another hf-mount supervisor already serves this session; exiting");
            return 0;
        }
        Err(e) => {
            error!("cannot create the control pipe {name}: {e}");
            return 1;
        }
    };
    registry.record(
        EventLevel::Info,
        None,
        format!("Supervisor started (pid {}, log {})", std::process::id(), log.display()),
    );
    if registry.host().elevated {
        registry.record(
            EventLevel::Warning,
            None,
            "Supervisor runs elevated: its drives are only visible to elevated programs, not to File Explorer",
        );
    }

    let watch = registry.clone();
    let stop = async move {
        loop {
            tokio::time::sleep(HEALTH_INTERVAL).await;
            let r = watch.clone();
            off_runtime(move || r.check_health()).await;
            if watch.shutdown_requested() {
                info!("shutdown requested");
                break;
            }
            if watch.active_count() == 0 && watch.idle_for() > IDLE_EXIT {
                info!("idle with no mounts; exiting");
                break;
            }
        }
        // Tear down while the control pipe still exists: clients see the
        // drives stopping, and a second supervisor cannot start (and fight
        // over the drive letters) until the uploads are finished or saved.
        watch.request_shutdown();
        let r = watch.clone();
        if let Some(reports) = off_runtime(move || r.unmount_all()).await {
            log_reports(&watch, &reports);
        }
    };
    let served = runtime.block_on(pipe::serve(
        name,
        server,
        sd,
        registry.clone(),
        identity.sid.clone(),
        stop,
    ));
    if let Err(e) = &served {
        error!("control pipe failed: {e}");
    }
    // Normally a no-op: `stop` already unmounted everything, unless the pipe
    // failed first.
    registry.request_shutdown();
    let reports = registry.unmount_all();
    log_reports(&registry, &reports);
    registry.record(EventLevel::Info, None, "Supervisor stopped");
    drop(runtime);
    if served.is_ok() { 0 } else { 1 }
}

fn log_reports(registry: &Registry, reports: &[UnmountReport]) {
    for r in reports {
        if let Some(summary) = r.incomplete_summary() {
            registry.record(EventLevel::Error, Some(&r.target), format!("At shutdown, {summary}"));
        }
    }
}

/// Outcome of a foreground mount, for the CLI to print.
pub enum ForegroundEvent<'a> {
    Mounted(&'a Response),
    Failed(&'a Response),
    Stopping(&'static str),
    Stopped(&'a [UnmountReport]),
}

/// `hf-mount start … --foreground`: mount in this process, serve the control
/// pipe (so `status`, `stop` and the app see it), and unmount on Ctrl+C,
/// console close, `hf-mount stop` or loss of the volume.
pub fn run_foreground(
    spec: MountSpec,
    credential: Option<Credential>,
    mut report: impl FnMut(ForegroundEvent<'_>),
) -> i32 {
    crate::setup::apply_xet_env_defaults();
    let identity = match Identity::current() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Error: cannot determine the current user: {e}");
            return 1;
        }
    };
    let settings = config::load_settings().unwrap_or_default();
    let runtime = crate::setup::build_runtime();
    let registry = Registry::new(
        mounter::mounter(runtime.handle().clone(), settings),
        host_info(HostKind::Foreground),
        None,
    );
    let name = identity.foreground_pipe(std::process::id());
    let bound = {
        let _guard = runtime.enter();
        pipe::bind(&name, &identity.sid, true)
    };
    let stop = Arc::new(tokio::sync::Notify::new());
    if let Ok((server, sd)) = bound {
        let stop = stop.clone();
        let registry = registry.clone();
        let sid = identity.sid.clone();
        runtime.spawn(async move {
            let _ = pipe::serve(name, server, sd, registry, sid, async move { stop.notified().await }).await;
        });
    } else {
        warn!("control pipe unavailable; `hf-mount status` will not list this mount");
    }

    let response = registry.handle(Request::Mount {
        spec: Box::new(spec),
        credential,
    });
    let code = match &response {
        Response::Mounted { .. } => {
            report(ForegroundEvent::Mounted(&response));
            0
        }
        Response::Error { error } => {
            report(ForegroundEvent::Failed(&response));
            stop.notify_waiters();
            return error.code.exit_code();
        }
        _ => 1,
    };

    let reason = runtime.block_on(async {
        let mut ctrl_close = tokio::signal::windows::ctrl_close().ok();
        let mut ctrl_break = tokio::signal::windows::ctrl_break().ok();
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return "Ctrl+C",
                _ = async { match ctrl_close.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => return "console closing",
                _ = async { match ctrl_break.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => return "Ctrl+Break",
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    let r = registry.clone();
                    off_runtime(move || r.check_health()).await;
                    if registry.shutdown_requested() {
                        return "shutdown requested";
                    }
                    if registry.active_count() == 0 {
                        return "unmounted";
                    }
                }
            }
        }
    });
    report(ForegroundEvent::Stopping(reason));
    let reports = registry.unmount_all();
    report(ForegroundEvent::Stopped(&reports));
    stop.notify_waiters();
    drop(runtime);
    if reports.iter().any(|r| !r.complete()) {
        crate::manager::protocol::ErrorCode::UploadsIncomplete.exit_code()
    } else {
        code
    }
}
