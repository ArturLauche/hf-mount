//! Messages exchanged between clients (CLI, desktop app) and mount hosts
//! (the background supervisor or a foreground `hf-mount start`).
//!
//! Framing: one JSON object per line (UTF-8, `\n`-terminated), at most
//! [`MAX_MESSAGE_BYTES`]. Every connection starts with `Hello`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::spec::MountSpec;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// How the client authenticates a mount. Never logged or persisted.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// A token value plus a label of where it came from (for status only).
    Token { token: String, source: String },
    /// A token file re-read before Hub requests (upstream `--token-file`).
    TokenFile { path: PathBuf },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Token { source, .. } => write!(f, "Token {{ source: {source:?}, token: *** }}"),
            Credential::TokenFile { path } => write!(f, "TokenFile {{ path: {path:?} }}"),
        }
    }
}

impl Credential {
    pub fn source_label(&self) -> String {
        match self {
            Credential::Token { source, .. } => source.clone(),
            Credential::TokenFile { path } => format!("token file {}", path.display()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
        client: String,
    },
    /// Mounts hosted by this host.
    List,
    Mount {
        spec: Box<MountSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<Credential>,
    },
    Unmount {
        target: String,
        /// Unmount even if applications hold files open for writing.
        #[serde(default)]
        force: bool,
    },
    /// Forget a failed mount record.
    Dismiss {
        target: String,
    },
    /// Re-queue failed uploads of a mount.
    RetryUploads {
        target: String,
    },
    /// Recent mount lifecycle events, newest last.
    Activity {
        #[serde(default = "default_activity_limit")]
        limit: usize,
    },
    /// Ask the host to exit after unmounting everything.
    Shutdown,
}

fn default_activity_limit() -> usize {
    200
}

/// Which kind of process hosts a mount.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// `hf-mount supervise`: background, survives terminals and the app.
    Supervisor,
    /// `hf-mount start --foreground`: lives as long as that console.
    Foreground,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub pid: u32,
    pub kind: HostKind,
    pub version: String,
    pub protocol: u32,
    /// RFC 3339.
    pub started_at: String,
    pub elevated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winfsp_version: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MountState {
    Starting,
    Mounted,
    Stopping,
    /// Mounting failed, or the volume was lost (see `error`).
    Failed,
}

/// Live counters for a mount. Only measured values; nothing estimated.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountStats {
    pub open_handles: usize,
    pub open_write_handles: usize,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub failed_operations: u64,
    /// Files written locally but not yet committed to the Hub.
    pub pending_uploads: usize,
    /// Remote deletes queued but not yet committed.
    pub pending_deletes: usize,
    /// Files whose last upload attempt failed (retried automatically).
    pub failed_uploads: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountInfo {
    pub id: String,
    pub spec: MountSpec,
    pub state: MountState,
    /// Source as resolved by the Hub (aliases expanded), once mounted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_source: Option<String>,
    /// Where the credential came from (never the credential itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
    pub host_pid: u32,
    pub host_kind: HostKind,
    /// RFC 3339.
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<MountStats>,
}

/// Stable, machine-readable error categories.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArgument,
    DriverMissing,
    DriverUnavailable,
    TargetInUse,
    AlreadyMounted,
    NotFound,
    AuthRequired,
    InvalidToken,
    Forbidden,
    Network,
    RateLimited,
    Timeout,
    NotMounted,
    BusyWithOpenFiles,
    UploadsIncomplete,
    HostUnavailable,
    ProtocolMismatch,
    Unsupported,
    Internal,
}

impl ErrorCode {
    /// Process exit code for the CLI. Documented in the README; keep stable.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::InvalidArgument | ErrorCode::Unsupported => 2,
            ErrorCode::DriverMissing | ErrorCode::DriverUnavailable => 3,
            ErrorCode::AuthRequired | ErrorCode::InvalidToken | ErrorCode::Forbidden => 4,
            ErrorCode::NotFound => 5,
            ErrorCode::TargetInUse | ErrorCode::AlreadyMounted => 6,
            ErrorCode::Network | ErrorCode::RateLimited | ErrorCode::Timeout => 7,
            ErrorCode::HostUnavailable | ErrorCode::ProtocolMismatch => 8,
            ErrorCode::NotMounted => 9,
            ErrorCode::BusyWithOpenFiles | ErrorCode::UploadsIncomplete => 10,
            ErrorCode::Internal => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::DriverMissing => "driver_missing",
            ErrorCode::DriverUnavailable => "driver_unavailable",
            ErrorCode::TargetInUse => "target_in_use",
            ErrorCode::AlreadyMounted => "already_mounted",
            ErrorCode::NotFound => "not_found",
            ErrorCode::AuthRequired => "auth_required",
            ErrorCode::InvalidToken => "invalid_token",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::Network => "network",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::Timeout => "timeout",
            ErrorCode::NotMounted => "not_mounted",
            ErrorCode::BusyWithOpenFiles => "busy_with_open_files",
            ErrorCode::UploadsIncomplete => "uploads_incomplete",
            ErrorCode::HostUnavailable => "host_unavailable",
            ErrorCode::ProtocolMismatch => "protocol_mismatch",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    /// One-sentence description (already redacted).
    pub message: String,
    /// What the user can do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ErrorInfo {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: super::redact::redact(&message.into()),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ErrorInfo {}

/// Result of stopping a mount.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnmountReport {
    pub target: String,
    /// Files whose final upload did not complete before the deadline.
    pub unflushed_files: usize,
    /// Where copies of those files were saved, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_to: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityEvent {
    /// RFC 3339.
    pub time: String,
    pub level: EventLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Hello { host: HostInfo },
    Mounts { host: HostInfo, mounts: Vec<MountInfo> },
    Mounted { mount: Box<MountInfo> },
    Unmounted { report: UnmountReport },
    Activity { events: Vec<ActivityEvent> },
    Ok,
    Error { error: ErrorInfo },
}

/// Serialise a message as one protocol line.
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol messages always serialise");
    line.push('\n');
    line
}

/// Parse one protocol line.
pub fn decode<'a, T: Deserialize<'a>>(line: &'a str) -> Result<T, ErrorInfo> {
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(ErrorInfo::new(ErrorCode::ProtocolMismatch, "message too large"));
    }
    serde_json::from_str(line.trim_end())
        .map_err(|e| ErrorInfo::new(ErrorCode::ProtocolMismatch, format!("malformed message: {e}")))
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::spec::MountSpec;

    #[test]
    fn requests_roundtrip_as_single_lines() {
        let spec = MountSpec::parse("repo", "openai-community/gpt2", "H:").unwrap();
        for req in [
            Request::Hello {
                protocol: PROTOCOL_VERSION,
                client: "cli".into(),
            },
            Request::List,
            Request::Mount {
                spec: Box::new(spec.clone()),
                credential: Some(Credential::Token {
                    token: "hf_secret".into(),
                    source: "HF_TOKEN".into(),
                }),
            },
            Request::Unmount {
                target: "H:".into(),
                force: false,
            },
            Request::Activity { limit: 10 },
            Request::Shutdown,
        ] {
            let line = encode(&req);
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(decode::<Request>(&line).unwrap(), req);
        }
        assert!(encode(&Request::List).contains(r#""op":"list""#));
    }

    #[test]
    fn credential_debug_never_shows_token() {
        let c = Credential::Token {
            token: "hf_abcdefghijklmnop".into(),
            source: "credential manager".into(),
        };
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("hf_abc"), "{dbg}");
        let req = Request::Mount {
            spec: Box::new(MountSpec::parse("bucket", "a/b", "X:").unwrap()),
            credential: Some(c),
        };
        assert!(!format!("{req:?}").contains("hf_abc"));
    }

    #[test]
    fn malformed_and_oversized_lines_are_errors() {
        assert_eq!(
            decode::<Request>("{nope").unwrap_err().code,
            ErrorCode::ProtocolMismatch
        );
        let huge = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(decode::<Request>(&huge).unwrap_err().code, ErrorCode::ProtocolMismatch);
    }

    #[test]
    fn error_messages_are_redacted() {
        let e = ErrorInfo::new(
            ErrorCode::Internal,
            "request with Bearer hf_abcdefghijklmnopqrstuv failed",
        );
        assert!(!e.message.contains("hf_abcdefghijklmnopqrstuv"));
    }

    #[test]
    fn exit_codes_are_distinct_per_category() {
        assert_eq!(ErrorCode::DriverMissing.exit_code(), 3);
        assert_eq!(ErrorCode::InvalidToken.exit_code(), 4);
        assert_eq!(ErrorCode::NotFound.exit_code(), 5);
        assert_eq!(ErrorCode::TargetInUse.exit_code(), 6);
        assert_eq!(ErrorCode::Network.exit_code(), 7);
        assert_eq!(ErrorCode::NotMounted.exit_code(), 9);
        assert!(ErrorCode::Internal.exit_code() != 0);
        let json = serde_json::to_string(&ErrorCode::BusyWithOpenFiles).unwrap();
        assert_eq!(json, format!("\"{}\"", ErrorCode::BusyWithOpenFiles.as_str()));
    }
}
