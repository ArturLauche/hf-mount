//! Client-side helpers shared by the CLI and the desktop app.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::{StreamExt, stream};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostSession {
    pub pid: u32,
    pub started_at: String,
    pub mounts: Vec<LostMount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostMount {
    pub target: String,
    pub source: String,
}

pub fn journal_path(identity: &Identity) -> PathBuf {
    state_dir().join(format!("supervisor-{}.json", identity.session))
}

/// Always try the well-known supervisor name even when enumeration of
/// foreground pipes is restricted.
pub(super) fn host_pipes(identity: &Identity) -> Vec<String> {
    let supervisor = identity.supervisor_pipe();
    let mut pipes = list_host_pipes(identity);
    pipes.retain(|name| name != &supervisor);
    pipes.insert(0, supervisor);
    pipes
}

async fn mounts_on(conn: &mut Connection) -> Result<(HostInfo, Vec<MountInfo>), ErrorInfo> {
    match conn.call(&Request::List).await? {
        Response::Mounts { host, mounts } => Ok((host, mounts)),
        other => Err(expect_error(other)),
    }
}

async fn list_host(
    identity: &Identity,
    pipe: String,
    existing: Option<Connection>,
) -> Result<Option<(HostSummary, Connection)>, ErrorInfo> {
    if let Some(mut conn) = existing {
        match mounts_on(&mut conn).await {
            Ok((host, mounts)) => return Ok(Some((HostSummary { pipe, host, mounts }, conn))),
            // Host exited/restarted or closed its idle pipe. Only a read is
            // replayed; the replacement goes through the usual peer checks.
            Err(e) if e.code == ErrorCode::HostUnavailable => {}
            Err(e) => return Err(e),
        }
    }
    let mut conn = match Connection::open(&pipe, identity).await {
        Ok(conn) => conn,
        Err(ConnectError::NotRunning) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let (host, mounts) = mounts_on(&mut conn).await?;
    Ok(Some((HostSummary { pipe, host, mounts }, conn)))
}

/// Verified, read-only control connections for a visible status consumer.
/// Discover the pipe names on every read; a connection never stands in for
/// a live response. Dropping the reader releases every pipe.
#[derive(Default)]
pub struct StatusConnections {
    pipes: HashMap<String, Connection>,
}

impl StatusConnections {
    pub async fn list(&mut self, identity: &Identity) -> (Vec<HostSummary>, Option<ErrorInfo>) {
        let mut previous = std::mem::take(&mut self.pipes);
        let queries = host_pipes(identity).into_iter().map(|pipe| {
            let existing = previous.remove(&pipe);
            list_host(identity, pipe, existing)
        });
        let mut queries = stream::iter(queries).buffered(4);
        let mut hosts = Vec::new();
        let mut error = None;
        while let Some(result) = queries.next().await {
            match result {
                Ok(Some((host, conn))) => {
                    self.pipes.insert(host.pipe.clone(), conn);
                    hosts.push(host);
                }
                Ok(None) => {}
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        (hosts, error)
    }
}

/// A one-shot query from the CLI or tray.
pub async fn list_hosts_with_error(identity: &Identity) -> (Vec<HostSummary>, Option<ErrorInfo>) {
    StatusConnections::default().list(identity).await
}

pub async fn list_hosts(identity: &Identity) -> Vec<HostSummary> {
    list_hosts_with_error(identity).await.0
}

/// A journal left behind by a supervisor that did not exit cleanly (no
/// supervisor pipe exists any more). Consumed: reported once, then removed.
pub fn take_lost_session(identity: &Identity) -> Option<LostSession> {
    let path = journal_path(identity);
    if !path.exists() || supervisor_pipe_exists(identity) {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    // Parse before deleting: an unreadable journal is kept for diagnosis
    // (the supervisor writes it atomically, so this should not happen).
    let journal: JournalFile = match serde_json::from_slice(&bytes) {
        Ok(journal) => journal,
        Err(e) => {
            tracing::warn!("unreadable session journal {}: {e}", path.display());
            return None;
        }
    };
    // Whoever removes it reports it; a concurrent caller sees it gone. If it
    // cannot be removed, keep reporting it rather than dropping the report.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => tracing::warn!("could not remove the session journal {}: {e}", path.display()),
    }
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
    for pipe in host_pipes(identity) {
        if let Ok(Some((summary, conn))) = list_host(identity, pipe, None).await
            && summary.mounts.iter().any(|m| m.spec.target == target)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::protocol::{HostKind, PROTOCOL_VERSION, decode, encode};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn visible_reader_reuses_handshake_and_detects_supervisor_restart() {
        let mut identity = Identity::current().unwrap();
        identity.session = uuid::Uuid::new_v4().as_u128() as u32;
        let pipe = identity.supervisor_pipe();
        let mut reader = StatusConnections::default();
        tokio::time::timeout(Duration::from_secs(10), async {
            let (hosts, error) = reader.list(&identity).await;
            assert!(hosts.is_empty() && error.is_none());

            // Each new host accepts just one connection. Two reads must share
            // that handshake and both must observe the live List responses.
            for incarnation in ["first", "replacement"] {
                let (server, _sd) = super::super::pipe::bind(&pipe, &identity.sid, true).unwrap();
                let (done, stop) = tokio::sync::oneshot::channel();
                let host = HostInfo {
                    kind: HostKind::Supervisor,
                    pid: std::process::id(),
                    started_at: incarnation.into(),
                    protocol: PROTOCOL_VERSION,
                    version: "test".into(),
                    elevated: false,
                    winfsp_version: None,
                };
                let serving = tokio::spawn(async move {
                    server.connect().await.unwrap();
                    let (read, mut write) = tokio::io::split(server);
                    let mut lines = BufReader::new(read).lines();
                    for i in 0..3 {
                        let line = lines.next_line().await.unwrap().unwrap();
                        let request = decode::<Request>(&line).unwrap();
                        let response = if i == 0 {
                            assert!(matches!(
                                request,
                                Request::Hello {
                                    protocol: PROTOCOL_VERSION,
                                    ..
                                }
                            ));
                            Response::Hello { host: host.clone() }
                        } else {
                            assert!(matches!(request, Request::List));
                            Response::Mounts {
                                host: host.clone(),
                                mounts: vec![],
                            }
                        };
                        write.write_all(encode(&response).as_bytes()).await.unwrap();
                    }
                    stop.await.unwrap();
                });
                for _ in 0..2 {
                    let (hosts, error) = reader.list(&identity).await;
                    assert!(error.is_none(), "{error:?}");
                    assert_eq!(hosts.len(), 1);
                    assert_eq!(hosts[0].host.started_at, incarnation);
                }
                done.send(()).unwrap();
                serving.await.unwrap();
                let (hosts, error) = reader.list(&identity).await;
                assert!(hosts.is_empty() && error.is_none(), "{error:?}");
            }
        })
        .await
        .expect("status reader did not complete");
    }

    #[tokio::test]
    async fn foreground_pipe_is_discovered_and_removed_without_restarting_the_reader() {
        let mut identity = Identity::current().unwrap();
        identity.session = uuid::Uuid::new_v4().as_u128() as u32;
        let pipe = identity.foreground_pipe(std::process::id());
        assert!(!host_pipes(&identity).contains(&pipe));
        let (server, _sd) = super::super::pipe::bind(&pipe, &identity.sid, true).unwrap();
        assert!(host_pipes(&identity).contains(&pipe));
        drop(server);
        assert!(!host_pipes(&identity).contains(&pipe));
    }
}
