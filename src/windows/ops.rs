//! High-level operations for front ends that are not the command line (the
//! desktop app). They talk to the same per-user supervisor through the same
//! control pipe as `hf-mount.exe`, so both see and manage the same drives.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::client::{self, LostSession};
use super::pipe::{Connection, Identity};
use super::{credman, drives, security, winfsp};
use crate::manager::auth::{self, Account, DEFAULT_ENDPOINT, Secret};
use crate::manager::config;
use crate::manager::protocol::{
    ActivityEvent, Credential, ErrorCode, ErrorInfo, HostInfo, HostKind, MountInfo, Request, Response, UnmountReport,
};
use crate::manager::spec::MountSpec;

/// State of the WinFsp driver on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverStatus {
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir: Option<String>,
    /// Set when WinFsp is registered but unusable (broken or too old).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Filled in by the front end: whether it ships the official installer.
    #[serde(default)]
    pub bundled_installer: bool,
}

pub fn driver_status() -> DriverStatus {
    match winfsp::api() {
        Ok(api) => DriverStatus {
            installed: true,
            version: api.version_string(),
            install_dir: Some(api.install_dir.display().to_string()),
            error: None,
            bundled_installer: false,
        },
        Err(winfsp::LoadError::NotInstalled) => DriverStatus {
            installed: false,
            version: None,
            install_dir: None,
            error: None,
            bundled_installer: false,
        },
        Err(e) => DriverStatus {
            installed: false,
            version: None,
            install_dir: winfsp::install_dir().map(|p| p.display().to_string()),
            error: Some(e.to_string()),
            bundled_installer: false,
        },
    }
}

/// Everything the drives screen shows, from every host of this session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    pub driver: DriverStatus,
    /// The background supervisor, when it is running.
    pub supervisor: Option<HostInfo>,
    /// Mounts of the supervisor and of foreground `hf-mount start` consoles.
    pub mounts: Vec<MountInfo>,
    pub lost: Option<LostSession>,
    /// This process runs elevated (its drives are invisible to Explorer).
    pub elevated: bool,
    pub free_letters: Vec<String>,
}

pub async fn overview(identity: &Identity) -> Overview {
    let hosts = client::list_hosts(identity).await;
    let supervisor = hosts
        .iter()
        .find(|h| h.host.kind == HostKind::Supervisor)
        .map(|h| h.host.clone());
    let mut mounts: Vec<MountInfo> = hosts.into_iter().flat_map(|h| h.mounts).collect();
    mounts.sort_by(|a, b| a.spec.target.cmp(&b.spec.target));
    Overview {
        driver: driver_status(),
        supervisor,
        mounts,
        lost: None,
        elevated: security::is_elevated(),
        free_letters: drives::free_letters().into_iter().map(|c| format!("{c}:")).collect(),
    }
}

/// Hub endpoint from the settings, or huggingface.co.
pub fn endpoint() -> String {
    config::load_settings()
        .ok()
        .and_then(|s| s.hub_endpoint)
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
}

/// The token stored in Windows Credential Manager for `endpoint`, as a
/// credential to send to the supervisor. `None` means anonymous access.
pub fn stored_credential(endpoint: &str) -> Option<Credential> {
    credman::read(endpoint)
        .ok()
        .flatten()
        .map(|(secret, _)| Credential::Token {
            token: secret.expose().to_string(),
            source: "Windows Credential Manager".to_string(),
        })
}

/// The stored token for the Hub that `spec` mounts from: its own Hub
/// address when it has one, else the one in the settings. Never a token
/// stored for a different Hub.
pub fn stored_credential_for(spec: &MountSpec) -> Option<Credential> {
    let endpoint = spec.tuning.hub_endpoint.clone().unwrap_or_else(endpoint);
    stored_credential(&endpoint)
}

/// Mount through the supervisor, starting it from `cli_exe` if needed.
pub async fn mount(
    identity: &Identity,
    cli_exe: &Path,
    spec: MountSpec,
    credential: Option<Credential>,
) -> Result<MountInfo, ErrorInfo> {
    let spec = spec
        .validated()
        .map_err(|e| ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string()))?;
    winfsp::api().map_err(|e| ErrorInfo::new(ErrorCode::DriverMissing, e.to_string()))?;
    let mut conn = client::ensure_supervisor(identity, cli_exe).await?;
    match conn
        .call(&Request::Mount {
            spec: Box::new(spec),
            credential,
        })
        .await?
    {
        Response::Mounted { mount } => Ok(*mount),
        other => Err(client::expect_error(other)),
    }
}

/// Send `request` to whichever host serves `target`.
async fn call_host_of(identity: &Identity, target: &str, request: Request) -> Result<Response, ErrorInfo> {
    let Some((_, mut conn)) = client::host_for_target(identity, target).await else {
        let exists = target.chars().next().is_some_and(drives::drive_exists);
        let message = if exists {
            format!("{target} is not an hf-mount drive of this session")
        } else {
            format!("nothing is mounted at {target}")
        };
        return Err(ErrorInfo::new(ErrorCode::NotMounted, message));
    };
    conn.call(&request).await
}

pub async fn unmount(identity: &Identity, target: &str, force: bool) -> Result<UnmountReport, ErrorInfo> {
    let request = Request::Unmount {
        target: target.to_string(),
        force,
    };
    match call_host_of(identity, target, request).await? {
        Response::Unmounted { report } => Ok(report),
        other => Err(client::expect_error(other)),
    }
}

/// Unmount every drive of this session. Errors are collected per drive, not
/// fatal. Without `force`, drives with files open for writing are left
/// mounted and reported with [`ErrorCode::BusyWithOpenFiles`], as a single
/// unmount would.
pub async fn unmount_all(identity: &Identity, force: bool) -> (Vec<UnmountReport>, Vec<(String, ErrorInfo)>) {
    let mut reports = Vec::new();
    let mut errors = Vec::new();
    for host in client::list_hosts(identity).await {
        for m in host.mounts {
            match unmount(identity, &m.spec.target, force).await {
                Ok(r) => reports.push(r),
                Err(e) if e.code == ErrorCode::NotMounted => {}
                Err(e) => errors.push((m.spec.target.clone(), e)),
            }
        }
    }
    (reports, errors)
}

async fn expect_ok(identity: &Identity, target: &str, request: Request) -> Result<(), ErrorInfo> {
    match call_host_of(identity, target, request).await? {
        Response::Ok => Ok(()),
        other => Err(client::expect_error(other)),
    }
}

pub async fn dismiss(identity: &Identity, target: &str) -> Result<(), ErrorInfo> {
    let request = Request::Dismiss {
        target: target.to_string(),
    };
    expect_ok(identity, target, request).await
}

pub async fn retry_uploads(identity: &Identity, target: &str) -> Result<(), ErrorInfo> {
    let request = Request::RetryUploads {
        target: target.to_string(),
    };
    expect_ok(identity, target, request).await
}

/// Recent events of every host, oldest first.
pub async fn activity(identity: &Identity, limit: usize) -> Vec<ActivityEvent> {
    let mut events = Vec::new();
    for host in client::list_hosts(identity).await {
        if let Ok(mut conn) = Connection::open(&host.pipe, identity).await
            && let Ok(Response::Activity { events: e }) = conn.call(&Request::Activity { limit }).await
        {
            events.extend(e);
        }
    }
    // RFC 3339 timestamps in UTC sort chronologically as strings.
    events.sort_by(|a, b| a.time.cmp(&b.time));
    let skip = events.len().saturating_sub(limit);
    events.split_off(skip)
}

/// Sign-in state as shown on the account screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatus {
    /// `signed_out`, `stored` (not checked), `signed_in`, `invalid_token`
    /// or `unverified` (the Hub could not be reached).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_account: Option<String>,
    /// `HF_TOKEN` or `HUGGING_FACE_HUB_TOKEN` when set for this user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_variable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

fn token_environment_variable() -> Option<String> {
    ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]
        .into_iter()
        .find(|v| std::env::var(v).is_ok_and(|t| !t.trim().is_empty()))
        .map(str::to_string)
}

pub async fn account_status(endpoint: &str, validate: bool) -> Result<AccountStatus, ErrorInfo> {
    let stored =
        credman::read(endpoint).map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("Credential Manager: {e}")))?;
    let environment_variable = token_environment_variable();
    let Some((secret, stored_account)) = stored else {
        return Ok(AccountStatus {
            state: "signed_out".into(),
            account: None,
            stored_account: None,
            environment_variable,
            error: None,
        });
    };
    let mut status = AccountStatus {
        state: "stored".into(),
        account: None,
        stored_account,
        environment_variable,
        error: None,
    };
    if validate {
        match auth::whoami(endpoint, secret.expose()).await {
            Ok(account) => {
                status.state = "signed_in".into();
                status.account = Some(account);
            }
            Err(e) if e.code == ErrorCode::InvalidToken => status.state = "invalid_token".into(),
            Err(e) => {
                status.state = "unverified".into();
                status.error = Some(e);
            }
        }
    }
    Ok(status)
}

/// Validate `token` with the Hub, then store it in Credential Manager.
pub async fn sign_in(endpoint: &str, token: &str) -> Result<AccountStatus, ErrorInfo> {
    let secret = Secret::new(token).ok_or_else(|| ErrorInfo::new(ErrorCode::InvalidArgument, "the token is empty"))?;
    let account = auth::whoami(endpoint, secret.expose()).await?;
    credman::store(endpoint, secret.expose(), Some(&account.name))
        .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("Credential Manager: {e}")))?;
    Ok(AccountStatus {
        state: "signed_in".into(),
        stored_account: Some(account.name.clone()),
        account: Some(account),
        environment_variable: token_environment_variable(),
        error: None,
    })
}

pub fn sign_out(endpoint: &str) -> Result<(), ErrorInfo> {
    credman::delete(endpoint)
        .map(|_| ())
        .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("Credential Manager: {e}")))
}

/// A plain-text report for bug reports. Contains no secrets: tokens are never
/// read into it and the log excerpt is already redacted when written; the
/// whole text is passed through the redactor once more regardless.
pub async fn diagnostics(identity: &Identity, app_version: &str) -> String {
    use std::fmt::Write;
    let ov = overview(identity).await;
    let settings = config::load_settings().unwrap_or_default();
    let endpoint = endpoint();
    let stored = credman::read(&endpoint).ok().flatten();
    let mut s = String::new();
    let _ = writeln!(s, "hf-mount diagnostics");
    let _ = writeln!(s, "app {app_version}, engine {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(s, "windows session {} elevated={}", identity.session, ov.elevated);
    let _ = writeln!(
        s,
        "winfsp installed={} version={} error={}",
        ov.driver.installed,
        ov.driver.version.as_deref().unwrap_or("-"),
        ov.driver.error.as_deref().unwrap_or("-")
    );
    match &ov.supervisor {
        Some(h) => {
            let _ = writeln!(
                s,
                "supervisor pid={} version={} since={}",
                h.pid, h.version, h.started_at
            );
        }
        None => {
            let _ = writeln!(s, "supervisor not running");
        }
    }
    let _ = writeln!(s, "hub endpoint {endpoint}; token stored={}", stored.is_some());
    let _ = writeln!(
        s,
        "cache {} max={} bytes; upload wait {} s; metadata ttl {} ms",
        settings.cache_root().display(),
        settings.cache_size_bytes,
        settings.upload_wait_secs,
        settings.metadata_ttl_ms
    );
    let _ = writeln!(s, "free letters {}", ov.free_letters.join(" "));
    let _ = writeln!(s, "\nmounts ({}):", ov.mounts.len());
    for m in &ov.mounts {
        let _ = writeln!(
            s,
            "  {} {} {:?} host={:?}/{} read_only={} stats={}",
            m.spec.target,
            m.spec.display_source(),
            m.state,
            m.host_kind,
            m.host_pid,
            m.spec.read_only,
            m.stats
                .as_ref()
                .map(|st| serde_json::to_string(st).unwrap_or_default())
                .unwrap_or_default()
        );
        if let Some(e) = &m.error {
            let _ = writeln!(s, "    error {}: {}", e.code.as_str(), e.message);
        }
    }
    let _ = writeln!(s, "\nrecent supervisor log:");
    for line in crate::manager::logging::tail(&config::logs_dir().join("supervisor.log"), 80) {
        let _ = writeln!(s, "  {line}");
    }
    crate::manager::redact::redact(&s)
}
