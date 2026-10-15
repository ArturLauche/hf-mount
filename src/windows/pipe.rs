//! Control pipes between clients (CLI, desktop app) and mount hosts.
//!
//! * Names: `\\.\pipe\hf-mount-<user SID>-<session>-<role>`; `role` is
//!   `supervisor` or `fg-<pid>`. Drive letters are per logon session, so the
//!   session is part of the name.
//! * The pipe DACL grants access to the current user only; remote clients
//!   are rejected; the supervisor pipe is created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a second supervisor (or another
//!   process squatting the name) cannot exist.
//! * Clients check that the server process belongs to the same user before
//!   sending anything, and servers check the client the same way.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};
use tracing::{debug, warn};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};

use super::security::{SecurityDescriptor, current_session_id, current_user_sid, pipe_sddl, process_user_sid};
use super::util::wide;
use crate::manager::protocol::{
    ErrorCode, ErrorInfo, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Request, Response, decode, encode,
};
use crate::manager::registry::Registry;

pub const SUPERVISOR_ROLE: &str = "supervisor";

/// Identity of the current user and session, used for pipe names and checks.
#[derive(Clone, Debug)]
pub struct Identity {
    pub sid: String,
    pub session: u32,
}

impl Identity {
    pub fn current() -> io::Result<Self> {
        Ok(Self {
            sid: current_user_sid()?,
            session: current_session_id(),
        })
    }

    /// Name prefix (without `\\.\pipe\`) shared by all of this user's hosts.
    pub fn prefix(&self) -> String {
        format!("hf-mount-{}-{}-", self.sid, self.session)
    }

    pub fn pipe_name(&self, role: &str) -> String {
        format!(r"\\.\pipe\{}{role}", self.prefix())
    }

    pub fn supervisor_pipe(&self) -> String {
        self.pipe_name(SUPERVISOR_ROLE)
    }

    pub fn foreground_pipe(&self, pid: u32) -> String {
        self.pipe_name(&format!("fg-{pid}"))
    }
}

/// Full names of this user's/session's host pipes currently in existence.
pub fn list_host_pipes(identity: &Identity) -> Vec<String> {
    let prefix = identity.prefix();
    let pattern = wide(r"\\.\pipe\*");
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: valid pattern and out struct.
    let handle = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
    if handle == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut names = Vec::new();
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        if let Ok(name) = String::from_utf16(&data.cFileName[..len])
            && name.starts_with(&prefix)
        {
            names.push(format!(r"\\.\pipe\{name}"));
        }
        // SAFETY: valid find handle.
        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            break;
        }
    }
    unsafe { FindClose(handle) };
    names.sort();
    names
}

/// Whether a pipe with this full name exists, without connecting to it.
pub fn pipe_exists(name: &str) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_SEM_TIMEOUT, GetLastError};
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    let w = wide(name);
    // SAFETY: valid NUL-terminated name; 1 ms timeout.
    if unsafe { WaitNamedPipeW(w.as_ptr(), 1) } != 0 {
        return true;
    }
    // Busy (all instances in use) still means it exists.
    let error = unsafe { GetLastError() };
    error == ERROR_SEM_TIMEOUT
}

fn create_server(name: &str, first: bool, sd: &SecurityDescriptor) -> io::Result<NamedPipeServer> {
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_ptr(),
        bInheritHandle: 0,
    };
    // SAFETY: `attrs` and the descriptor it points to outlive the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(64)
            .create_with_security_attributes_raw(name, (&mut attrs as *mut SECURITY_ATTRIBUTES).cast::<c_void>())
    }
}

/// Create the first instance of a host pipe. For the supervisor this fails
/// with `AccessDenied` when another supervisor already owns the name.
pub fn bind(name: &str, sid: &str, first_instance: bool) -> io::Result<(NamedPipeServer, Arc<SecurityDescriptor>)> {
    let sd = Arc::new(SecurityDescriptor::from_sddl(&pipe_sddl(sid))?);
    let server = create_server(name, first_instance, &sd)?;
    Ok((server, sd))
}

fn peer_matches(sid: &str, pid: Option<u32>) -> bool {
    match pid.map(process_user_sid) {
        Some(Ok(peer)) => peer == sid,
        _ => false,
    }
}

async fn read_message(reader: &mut BufReader<ReadHalf<impl tokio::io::AsyncRead>>) -> io::Result<Option<String>> {
    let mut line = String::new();
    let n = (&mut *reader)
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_line(&mut line)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_MESSAGE_BYTES || !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large or truncated",
        ));
    }
    Ok(Some(line))
}

/// Run a registry request on a plain OS thread: mounting blocks on the
/// runtime and on WinFsp, so it must not run on a tokio worker.
async fn dispatch(registry: Arc<Registry>, request: Request) -> Response {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("hf-mount-request".into())
        .spawn(move || {
            let _ = tx.send(registry.handle(request));
        })
        .expect("spawn request thread");
    rx.await.unwrap_or(Response::Error {
        error: ErrorInfo::new(ErrorCode::Internal, "request handler panicked"),
    })
}

async fn serve_connection(pipe: NamedPipeServer, registry: Arc<Registry>, sid: String) {
    let mut client_pid = 0u32;
    // SAFETY: valid pipe handle.
    let ok = unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut client_pid) } != 0;
    if !peer_matches(&sid, ok.then_some(client_pid)) {
        warn!("rejected control connection from pid {client_pid}: not the mounting user");
        return;
    }
    let (r, mut w) = tokio::io::split(pipe);
    let mut reader = BufReader::new(r);
    loop {
        // An idle client must not hold one of the limited pipe instances.
        let line = match tokio::time::timeout(SERVER_IDLE_TIMEOUT, read_message(&mut reader)).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                debug!("control connection closed: {e}");
                return;
            }
            Err(_) => {
                debug!("closing idle control connection");
                return;
            }
        };
        let response = match decode::<Request>(&line) {
            Ok(request) => dispatch(registry.clone(), request).await,
            Err(error) => Response::Error { error },
        };
        if w.write_all(encode(&response).as_bytes()).await.is_err() || w.flush().await.is_err() {
            return;
        }
    }
}

/// Create the next listening instance, retrying while the instance limit is
/// reached or creation fails transiently. Returns `None` once `stop` fires.
async fn next_instance(
    name: &str,
    sd: &SecurityDescriptor,
    stop: &mut std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
) -> Option<NamedPipeServer> {
    let mut delay = Duration::from_millis(50);
    loop {
        match create_server(name, false, sd) {
            Ok(server) => return Some(server),
            Err(e) => warn!("could not create a control pipe instance (retrying): {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stop.as_mut() => return None,
        }
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

/// Accept control connections until `stop` resolves. Errors on a single
/// connection are logged and never end the loop: the mounts this host serves
/// must not go away because one client misbehaved.
pub async fn serve(
    name: String,
    mut server: NamedPipeServer,
    sd: Arc<SecurityDescriptor>,
    registry: Arc<Registry>,
    sid: String,
    stop: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    tokio::pin!(stop);
    loop {
        tokio::select! {
            res = server.connect() => {
                // The next instance is created before the connected one is
                // handed off, so the name always exists: a client never sees
                // "not running" and a second supervisor can never bind it.
                let Some(next) = next_instance(&name, &sd, &mut stop).await else {
                    return Ok(());
                };
                let connected = std::mem::replace(&mut server, next);
                match res {
                    Ok(()) => {
                        tokio::spawn(serve_connection(connected, registry.clone(), sid.clone()));
                    }
                    // The client went away before it was accepted, or the
                    // instance broke; drop it and keep listening.
                    Err(e) => debug!("control pipe connect failed: {e}"),
                }
            }
            _ = &mut stop => return Ok(()),
        }
    }
}

/// Why a client could not talk to a host.
#[derive(Debug)]
pub enum ConnectError {
    /// No host listens on that name.
    NotRunning,
    /// The pipe exists but belongs to another user: refuse to talk to it.
    Untrusted,
    Io(io::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::NotRunning => f.write_str("the hf-mount supervisor is not running"),
            ConnectError::Untrusted => f.write_str("the control pipe is owned by another user; refusing to use it"),
            ConnectError::Io(e) => write!(f, "control pipe error: {e}"),
        }
    }
}

impl From<ConnectError> for ErrorInfo {
    fn from(e: ConnectError) -> Self {
        let code = match e {
            ConnectError::NotRunning => ErrorCode::HostUnavailable,
            ConnectError::Untrusted => ErrorCode::HostUnavailable,
            ConnectError::Io(_) => ErrorCode::HostUnavailable,
        };
        ErrorInfo::new(code, e.to_string())
    }
}

/// How long a host may take to answer `request`. Mounting includes Hub
/// round trips with retries; unmounting waits for pending uploads (the
/// "upload wait" setting); everything else is answered from memory.
pub fn request_timeout(request: &Request) -> Duration {
    match request {
        Request::Mount { .. } => Duration::from_secs(5 * 60),
        Request::Unmount { .. } => {
            let upload_wait = crate::manager::config::load_settings()
                .map(|s| s.upload_wait_secs)
                .unwrap_or(300);
            Duration::from_secs(upload_wait.saturating_add(5 * 60))
        }
        _ => Duration::from_secs(30),
    }
}

/// Idle time after which a host closes a control connection.
const SERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// An open, verified control connection.
pub struct Connection {
    reader: BufReader<ReadHalf<NamedPipeClient>>,
    writer: WriteHalf<NamedPipeClient>,
    pub server_pid: u32,
}

impl Connection {
    pub async fn open(name: &str, identity: &Identity) -> Result<Self, ConnectError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let client = loop {
            match ClientOptions::new().open(name) {
                Ok(client) => break client,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    return Err(ConnectError::NotRunning);
                }
                Err(e) => return Err(ConnectError::Io(e)),
            }
        };
        let mut server_pid = 0u32;
        // SAFETY: valid pipe handle.
        let ok = unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut server_pid) } != 0;
        if !peer_matches(&identity.sid, ok.then_some(server_pid)) {
            return Err(ConnectError::Untrusted);
        }
        let (r, w) = tokio::io::split(client);
        let mut conn = Self {
            reader: BufReader::new(r),
            writer: w,
            server_pid,
        };
        match conn
            .call(&Request::Hello {
                protocol: PROTOCOL_VERSION,
                client: format!("hf-mount {}", env!("CARGO_PKG_VERSION")),
            })
            .await
        {
            Ok(Response::Hello { .. }) => Ok(conn),
            Ok(Response::Error { error }) => Err(ConnectError::Io(io::Error::other(error.to_string()))),
            Ok(_) => Err(ConnectError::Io(io::Error::other("unexpected hello response"))),
            Err(e) => Err(ConnectError::Io(io::Error::other(e.to_string()))),
        }
    }

    /// Send one request and wait for its response, at most as long as the
    /// request can legitimately take (see [`request_timeout`]).
    pub async fn call(&mut self, request: &Request) -> Result<Response, ErrorInfo> {
        let limit = request_timeout(request);
        match tokio::time::timeout(limit, self.exchange(request)).await {
            Ok(result) => result,
            Err(_) => Err(ErrorInfo::new(
                ErrorCode::Timeout,
                format!("the mount host did not answer within {} s", limit.as_secs()),
            )
            .with_hint("it may still be working; check `hf-mount status`, or see the supervisor log")),
        }
    }

    async fn exchange(&mut self, request: &Request) -> Result<Response, ErrorInfo> {
        let io_err = |e: io::Error| ErrorInfo::new(ErrorCode::HostUnavailable, format!("control pipe: {e}"));
        self.writer
            .write_all(encode(request).as_bytes())
            .await
            .map_err(io_err)?;
        self.writer.flush().await.map_err(io_err)?;
        match read_message(&mut self.reader).await.map_err(io_err)? {
            Some(line) => decode(&line),
            None => Err(ErrorInfo::new(
                ErrorCode::HostUnavailable,
                "the mount host closed the connection (it may have exited)",
            )),
        }
    }
}
