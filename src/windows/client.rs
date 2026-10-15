//! Client-side helpers shared by the CLI and the desktop app.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::pipe::{ConnectError, Connection, Identity, list_host_pipes};
use crate::manager::config::state_dir;
use crate::manager::protocol::{ErrorCode, ErrorInfo, HostInfo, MountInfo, Request, Response};
use crate::manager::registry::JournalFile;

/// One mount host and its mounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSummary {
    pub pipe: String,
    pub host: HostInfo,
    pub mounts: Vec<MountInfo>,
}

/// Mounts that disappeared because their supervisor exited abnormally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LostSession {
    pub pid: u32,
    pub started_at: String,
    pub mounts: Vec<LostMount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LostMount {
    pub target: String,
    pub source: String,
}

pub fn journal_path(identity: &Identity) -> PathBuf {
    state_dir().join(format!("supervisor-{}.json", identity.session))
}

/// Query every host of this user/session.
pub async fn list_hosts(identity: &Identity) -> Vec<HostSummary> {
    let mut out = Vec::new();
    // The supervisor's name is well known; enumeration (which some
    // environments restrict) is only needed to find foreground hosts.
    let mut pipes = vec![identity.supervisor_pipe()];
    for pipe in list_host_pipes(identity) {
        if !pipes.contains(&pipe) {
            pipes.push(pipe);
        }
    }
    for pipe in pipes {
        let Ok(mut conn) = Connection::open(&pipe, identity).await else {
            continue;
        };
        if let Ok(Response::Mounts { host, mounts }) = conn.call(&Request::List).await {
            out.push(HostSummary { pipe, host, mounts });
        }
    }
    out
}

/// A journal left behind by a supervisor that did not exit cleanly (no
/// supervisor pipe exists any more). Consumed: reported once, then removed.
pub fn take_lost_session(identity: &Identity) -> Option<LostSession> {
    let path = journal_path(identity);
    if !path.exists() || supervisor_pipe_exists(identity) {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let journal: JournalFile = serde_json::from_slice(&bytes).ok()?;
    if journal.mounts.is_empty() {
        return None;
    }
    Some(LostSession {
        pid: journal.pid,
        started_at: journal.started_at,
        mounts: journal
            .mounts
            .into_iter()
            .map(|m| LostMount {
                target: m.target,
                source: m.source,
            })
            .collect(),
    })
}

/// Whether the supervisor's pipe currently exists (without connecting).
fn supervisor_pipe_exists(identity: &Identity) -> bool {
    super::pipe::pipe_exists(&identity.supervisor_pipe())
}

/// Connect to the supervisor, starting it from `exe` if it is not running.
pub async fn ensure_supervisor(identity: &Identity, exe: &Path) -> Result<Connection, ErrorInfo> {
    let name = identity.supervisor_pipe();
    match Connection::open(&name, identity).await {
        Ok(conn) => return Ok(conn),
        Err(ConnectError::NotRunning) => {}
        Err(e) => return Err(e.into()),
    }
    super::process::spawn_supervisor(exe).map_err(|e| {
        ErrorInfo::new(
            ErrorCode::HostUnavailable,
            format!("could not start the hf-mount supervisor ({}): {e}", exe.display()),
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match Connection::open(&name, identity).await {
            Ok(conn) => return Ok(conn),
            Err(ConnectError::NotRunning) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => {
                return Err(ErrorInfo::new(
                    ErrorCode::HostUnavailable,
                    format!("the hf-mount supervisor did not start: {e}"),
                )
                .with_hint("see the supervisor log (hf-mount doctor shows its location)"));
            }
        }
    }
}

/// The host that serves `target`, with an open connection to it.
pub async fn host_for_target(identity: &Identity, target: &str) -> Option<(HostSummary, Connection)> {
    for summary in list_hosts(identity).await {
        if summary.mounts.iter().any(|m| m.spec.target == target)
            && let Ok(conn) = Connection::open(&summary.pipe, identity).await
        {
            return Some((summary, conn));
        }
    }
    None
}

/// Unwrap an expected response variant or turn it into an error.
pub fn expect_error(resp: Response) -> ErrorInfo {
    match resp {
        Response::Error { error } => error,
        other => ErrorInfo::new(ErrorCode::ProtocolMismatch, format!("unexpected response: {other:?}")),
    }
}
