//! `hf-mount.exe` — the Windows command line.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};

use super::client::{self, HostSummary, LostSession};
use super::pipe::{ConnectError, Connection, Identity};
use super::{credman, drives, host, security, winfsp};
use crate::manager::auth::{self, DEFAULT_ENDPOINT, Secret, TokenSources};
use crate::manager::config::{self, logs_dir};
use crate::manager::protocol::{
    Credential, ErrorCode, ErrorInfo, MountInfo, MountState, Request, Response, UnmountReport,
};
use crate::manager::spec::{MountSpec, normalize_target};

const WINFSP_NOTICE: &str = "WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos \
                             <https://github.com/winfsp/winfsp>";

fn long_version() -> &'static str {
    Box::leak(
        format!(
            "{} (Windows edition)\nNative drives are provided by {WINFSP_NOTICE}.\nLicense: Apache-2.0",
            env!("CARGO_PKG_VERSION")
        )
        .into_boxed_str(),
    )
}

const EXIT_CODES_HELP: &str = "\
Exit codes:
  0  success                    6  drive letter in use / already mounted
  1  internal error             7  network, rate limit or timeout
  2  invalid arguments          8  supervisor unavailable
  3  WinFsp missing/unavailable 9  not mounted
  4  authentication/permission  10 open files or uploads not finished
  5  repository/bucket not found";

#[derive(Parser)]
#[command(
    name = "hf-mount",
    version,
    long_version = long_version(),
    about = "Mount Hugging Face repositories and buckets as Windows drives",
    after_help = EXIT_CODES_HELP
)]
struct Cli {
    /// Print machine-readable JSON on stdout.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

// Parsed once per process; the variant size difference is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Mount a repository or bucket on a drive letter (background by default)
    ///
    /// Examples:
    ///   hf-mount start repo openai-community/gpt2 H:
    ///   hf-mount start repo datasets/user/dataset-name D: --revision main
    ///   hf-mount start bucket user/my-bucket X:
    #[command(verbatim_doc_comment)]
    Start(StartArgs),
    /// Unmount a drive (waits for pending uploads).
    Stop(StopArgs),
    /// List mounts managed by hf-mount for this user and session.
    Status,
    /// Store, check or remove the Hugging Face token (Windows Credential Manager).
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Check WinFsp, the supervisor, drive letters and connectivity.
    Doctor,
    /// List free drive letters.
    Drives,
    /// Show recent mount activity from the background supervisor.
    Activity {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Re-queue failed uploads of a bucket mount.
    RetryUploads { target: String },
    /// Run the background mount supervisor (started automatically).
    #[command(hide = true)]
    Supervise,
}

#[derive(Args)]
struct StartArgs {
    /// Source type: repo (type from a datasets/ or spaces/ prefix), model, dataset, space or bucket.
    kind: String,
    /// Repository or bucket id, e.g. openai-community/gpt2, user/bucket; may end with /sub/folder.
    id: String,
    /// Drive letter, e.g. H:
    target: String,
    /// Branch, tag or commit to mount (repositories only; default main).
    #[arg(long)]
    revision: Option<String>,
    /// Only expose this folder of the repository or bucket.
    #[arg(long)]
    subfolder: Option<String>,
    /// Mount a bucket read-only (repositories are always read-only).
    #[arg(long)]
    read_only: bool,
    /// Keep the mount in this console; Ctrl+C unmounts.
    #[arg(long)]
    foreground: bool,
    /// Read the token from this file (re-read before Hub requests).
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Token on the command line. Discouraged: other processes of your user
    /// can read command lines. Prefer `hf-mount auth login` or HF_TOKEN.
    #[arg(long, hide_env_values = true)]
    hf_token: Option<String>,
    /// Ignore stored and environment tokens (public repositories only).
    #[arg(long, conflicts_with_all = ["token_file", "hf_token"])]
    anonymous: bool,
    /// Cache root directory (a subfolder per source is used).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Maximum cache size, e.g. 20GB or 500MB.
    #[arg(long, value_parser = parse_size)]
    cache_size: Option<u64>,
    /// Do not cache file data on disk.
    #[arg(long)]
    no_disk_cache: bool,
    /// How long file metadata is trusted before re-checking the Hub (ms).
    #[arg(long)]
    metadata_ttl_ms: Option<u64>,
    /// Interval for polling remote changes in seconds (0 disables).
    #[arg(long)]
    poll_interval_secs: Option<u64>,
    /// Hub endpoint (default https://huggingface.co).
    #[arg(long)]
    hub_endpoint: Option<String>,
    /// Open the drive in File Explorer after mounting.
    #[arg(long)]
    open: bool,
}

#[derive(Args)]
struct StopArgs {
    /// Drive letter to unmount, e.g. H:
    #[arg(required_unless_present = "all")]
    target: Option<String>,
    /// Unmount every hf-mount drive of this session.
    #[arg(long, conflicts_with = "target")]
    all: bool,
    /// Unmount even if applications hold files open for writing.
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Store a token in Windows Credential Manager (validated against the Hub).
    Login {
        /// Read the token from standard input instead of prompting.
        #[arg(long)]
        token_stdin: bool,
        /// Store without contacting the Hub.
        #[arg(long)]
        no_validate: bool,
        #[arg(long)]
        hub_endpoint: Option<String>,
    },
    /// Show which token would be used and whether it is valid.
    Status {
        /// Do not contact the Hub.
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        hub_endpoint: Option<String>,
    },
    /// Remove the stored token.
    Logout {
        #[arg(long)]
        hub_endpoint: Option<String>,
    },
}

/// Parse `500MB`, `20GB`, `1GiB` or plain bytes.
fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num.parse().map_err(|_| format!("{s:?} is not a size"))?;
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "kib" => 1024.0,
        "m" | "mb" => 1e6,
        "mib" => 1024.0 * 1024.0,
        "g" | "gb" => 1e9,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" => 1e12,
        "tib" => 1024.0f64.powi(4),
        other => return Err(format!("unknown size unit {other:?}")),
    };
    let bytes = value * mult;
    if !(bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64) {
        return Err(format!("{s:?} is out of range"));
    }
    Ok(bytes as u64)
}

struct Out {
    json: bool,
}

impl Out {
    fn json(&self, value: Value) {
        println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
    }

    fn fail(&self, error: &ErrorInfo) -> i32 {
        let code = error.code.exit_code();
        if self.json {
            self.json(json!({
                "ok": false,
                "error": {
                    "code": error.code,
                    "message": error.message,
                    "hint": error.hint,
                    "exit_code": code,
                }
            }));
        } else {
            eprintln!("Error: {}", error.message);
            if let Some(hint) = &error.hint {
                eprintln!("  {hint}");
            }
        }
        code
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn identity() -> Result<Identity, ErrorInfo> {
    Identity::current()
        .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("cannot determine the current user: {e}")))
}

fn endpoint_or_default(endpoint: Option<&str>) -> String {
    endpoint
        .map(str::to_string)
        .or_else(|| config::load_settings().ok().and_then(|s| s.hub_endpoint))
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
}

fn warn_if_elevated(out: &Out) {
    if security::is_elevated() && !out.json {
        eprintln!(
            "Warning: this console is elevated (Run as administrator). Drives mounted from an elevated \
             session are not visible in the normal File Explorer. Use a non-elevated console."
        );
    }
}

/// Entry point for the Windows `hf-mount` binary.
pub fn main() -> i32 {
    let cli = Cli::parse();
    let out = Out { json: cli.json };
    let result = match cli.command {
        Command::Supervise => return host::run_supervisor(),
        Command::Start(args) => start(&out, args),
        Command::Stop(args) => stop(&out, args),
        Command::Status => status(&out),
        Command::Auth(cmd) => auth_cmd(&out, cmd),
        Command::Doctor => doctor(&out),
        Command::Drives => drives_cmd(&out),
        Command::Activity { limit } => activity(&out, limit),
        Command::RetryUploads { target } => retry_uploads(&out, &target),
    };
    match result {
        Ok(code) => code,
        Err(error) => out.fail(&error),
    }
}

fn invalid(e: impl ToString) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::InvalidArgument, e.to_string())
}

fn build_spec(args: &StartArgs) -> Result<MountSpec, ErrorInfo> {
    let mut spec = MountSpec::parse(&args.kind, &args.id, &args.target).map_err(invalid)?;
    if let Some(sub) = &args.subfolder {
        if spec.subfolder.is_some() {
            return Err(invalid(
                "give the subfolder either in the id or with --subfolder, not both",
            ));
        }
        spec.subfolder = Some(sub.clone());
    }
    spec.revision = args.revision.clone();
    if args.read_only {
        spec.read_only = true;
    }
    spec.tuning.cache_dir = args.cache_dir.clone();
    spec.tuning.cache_size_bytes = args.cache_size;
    spec.tuning.no_disk_cache = args.no_disk_cache;
    spec.tuning.metadata_ttl_ms = args.metadata_ttl_ms;
    spec.tuning.poll_interval_secs = args.poll_interval_secs;
    spec.tuning.hub_endpoint = args.hub_endpoint.clone();
    spec.validated().map_err(invalid)
}

fn resolve_credential(args: &StartArgs, endpoint: &str) -> Option<(Credential, String)> {
    if args.anonymous {
        return None;
    }
    let endpoint = endpoint.to_string();
    let store = move || credman::read(&endpoint).ok().flatten().map(|(secret, _)| secret);
    auth::resolve_token(&TokenSources {
        token_file: args.token_file.as_deref(),
        command_line: args.hf_token.as_deref(),
        use_environment: true,
        credential_store: Some(&store),
        use_hf_cli: true,
    })
    .map(|(credential, source)| (credential, source.label()))
}

fn open_explorer(target: &str) {
    // `target` is a validated drive letter (`X:`), never user text.
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("{target}\\"))
        .spawn();
}

fn print_mounted(mount: &MountInfo) {
    let access = if mount.spec.read_only {
        "read-only"
    } else {
        "read-write"
    };
    println!(
        "Mounted {} at {} ({access})",
        mount
            .resolved_source
            .clone()
            .unwrap_or_else(|| mount.spec.display_source()),
        mount.spec.target
    );
    if let Some(src) = &mount.credential_source {
        println!("  Authenticated with: {src}");
    }
    if !mount.spec.read_only {
        println!(
            "  Writes are saved locally first and uploaded in the background; \
             `hf-mount status` shows pending uploads."
        );
    }
    println!(
        "  Open: explorer {}\\    Stop: hf-mount stop {}",
        mount.spec.target, mount.spec.target
    );
}

fn start(out: &Out, args: StartArgs) -> Result<i32, ErrorInfo> {
    let spec = build_spec(&args)?;
    if args.hf_token.is_some() && !out.json {
        eprintln!(
            "Warning: --hf-token puts the token on the command line, where other programs running as \
             you can read it. Prefer `hf-mount auth login` or the HF_TOKEN environment variable."
        );
    }
    warn_if_elevated(out);
    winfsp::api().map_err(|e| {
        ErrorInfo::new(ErrorCode::DriverMissing, e.to_string()).with_hint("run `hf-mount doctor` for installation help")
    })?;
    let endpoint = endpoint_or_default(spec.tuning.hub_endpoint.as_deref());
    let credential = resolve_credential(&args, &endpoint);

    if args.foreground {
        let json = out.json;
        let open = args.open;
        let code = host::run_foreground(spec, credential.map(|(c, _)| c), |event| match event {
            host::ForegroundEvent::Mounted(Response::Mounted { mount }) => {
                if json {
                    println!("{}", json!({"ok": true, "event": "mounted", "mount": mount}));
                } else {
                    print_mounted(mount);
                    println!("  Running in the foreground; press Ctrl+C to unmount.");
                }
                if open {
                    open_explorer(&mount.spec.target);
                }
            }
            host::ForegroundEvent::Failed(Response::Error { error }) => {
                Out { json }.fail(error);
            }
            host::ForegroundEvent::Stopping(reason) => {
                if !json {
                    eprintln!("Unmounting ({reason}); waiting for pending uploads…");
                }
            }
            host::ForegroundEvent::Stopped(reports) => {
                if json {
                    println!("{}", json!({"ok": true, "event": "unmounted", "reports": reports}));
                } else {
                    for r in reports {
                        print_report(r);
                    }
                }
            }
            _ => {}
        });
        return Ok(code);
    }

    if !out.json {
        eprintln!("Mounting {} at {}…", spec.display_source(), spec.target);
    }
    let identity = identity()?;
    let exe = std::env::current_exe().map_err(|e| ErrorInfo::new(ErrorCode::Internal, e.to_string()))?;
    let response = runtime().block_on(async {
        let mut conn = client::ensure_supervisor(&identity, &exe).await?;
        conn.call(&Request::Mount {
            spec: Box::new(spec),
            credential: credential.map(|(c, _)| c),
        })
        .await
    })?;
    match response {
        Response::Mounted { mount } => {
            if out.json {
                out.json(json!({"ok": true, "mount": mount}));
            } else {
                print_mounted(&mount);
            }
            if args.open {
                open_explorer(&mount.spec.target);
            }
            Ok(0)
        }
        other => Err(client::expect_error(other)),
    }
}

fn print_report(r: &UnmountReport) {
    if r.unflushed_files == 0 {
        println!("Unmounted {}", r.target);
    } else {
        println!(
            "Unmounted {} — {} file(s) were NOT uploaded to the Hub.",
            r.target, r.unflushed_files
        );
    }
    if let Some(dir) = &r.recovered_to {
        println!("  Copies of the unsaved files: {}", dir.display());
    }
    for w in &r.warnings {
        println!("  Note: {w}");
    }
}

fn stop(out: &Out, args: StopArgs) -> Result<i32, ErrorInfo> {
    let identity = identity()?;
    let rt = runtime();
    let hosts = rt.block_on(client::list_hosts(&identity));
    let targets: Vec<String> = if args.all {
        hosts
            .iter()
            .flat_map(|h| h.mounts.iter().map(|m| m.spec.target.clone()))
            .collect()
    } else {
        vec![normalize_target(args.target.as_deref().unwrap_or_default()).map_err(invalid)?]
    };
    if targets.is_empty() {
        if out.json {
            out.json(json!({"ok": true, "reports": []}));
        } else {
            println!("Nothing to stop.");
        }
        return Ok(0);
    }
    let mut reports = Vec::new();
    let mut worst: Option<ErrorInfo> = None;
    for target in targets {
        let Some(host) = hosts.iter().find(|h| h.mounts.iter().any(|m| m.spec.target == target)) else {
            let message = if drives::drive_exists(target.chars().next().unwrap_or('?')) {
                format!("{target} is not an hf-mount drive of this session")
            } else {
                format!("nothing is mounted at {target}")
            };
            worst.get_or_insert(ErrorInfo::new(ErrorCode::NotMounted, message));
            continue;
        };
        if !out.json {
            eprintln!("Unmounting {target}…");
        }
        let response = rt.block_on(async {
            let mut conn = Connection::open(&host.pipe, &identity).await.map_err(ErrorInfo::from)?;
            conn.call(&Request::Unmount {
                target: target.clone(),
                force: args.force,
            })
            .await
        });
        match response {
            Ok(Response::Unmounted { report }) => {
                if report.unflushed_files > 0 {
                    worst.get_or_insert(ErrorInfo::new(
                        ErrorCode::UploadsIncomplete,
                        format!(
                            "{} file(s) on {} were not uploaded",
                            report.unflushed_files, report.target
                        ),
                    ));
                }
                if !out.json {
                    print_report(&report);
                }
                reports.push(report);
            }
            Ok(other) => {
                worst.get_or_insert(client::expect_error(other));
            }
            Err(e) => {
                worst.get_or_insert(e);
            }
        }
    }
    if out.json {
        out.json(json!({
            "ok": worst.is_none(),
            "reports": reports,
            "error": worst.as_ref().map(|e| json!({"code": e.code, "message": e.message, "hint": e.hint, "exit_code": e.code.exit_code()})),
        }));
        return Ok(worst.map(|e| e.code.exit_code()).unwrap_or(0));
    }
    match worst {
        Some(e) => Err(e),
        None => Ok(0),
    }
}

fn state_label(state: MountState) -> &'static str {
    match state {
        MountState::Starting => "starting",
        MountState::Mounted => "mounted",
        MountState::Stopping => "stopping",
        MountState::Failed => "failed",
    }
}

fn print_lost(lost: &LostSession) {
    println!(
        "Note: the background supervisor (pid {}) exited unexpectedly; these mounts were lost:",
        lost.pid
    );
    for m in &lost.mounts {
        println!("  {}  {}", m.target, m.source);
    }
}

fn status(out: &Out) -> Result<i32, ErrorInfo> {
    let identity = identity()?;
    let hosts: Vec<HostSummary> = runtime().block_on(client::list_hosts(&identity));
    let lost = client::take_lost_session(&identity);
    if out.json {
        let mounts: Vec<&MountInfo> = hosts.iter().flat_map(|h| h.mounts.iter()).collect();
        out.json(json!({
            "ok": true,
            "hosts": hosts.iter().map(|h| json!({"pipe": h.pipe, "host": h.host})).collect::<Vec<_>>(),
            "mounts": mounts,
            "lost": lost,
        }));
        return Ok(0);
    }
    if let Some(lost) = &lost {
        print_lost(lost);
    }
    let rows: Vec<(&HostSummary, &MountInfo)> = hosts
        .iter()
        .flat_map(|h| h.mounts.iter().map(move |m| (h, m)))
        .collect();
    if rows.is_empty() {
        println!("No hf-mount drives in this session.");
        return Ok(0);
    }
    println!(
        "{:<7}{:<10}{:<7}{:<52}{:<10}HOST",
        "DRIVE", "STATE", "MODE", "SOURCE", "UPLOADS"
    );
    for (h, m) in rows {
        let mode = if m.spec.read_only { "ro" } else { "rw" };
        let source = m.resolved_source.clone().unwrap_or_else(|| m.spec.display_source());
        let uploads = match &m.stats {
            Some(s) if s.failed_uploads > 0 => format!("{} failed", s.failed_uploads),
            Some(s) if s.pending_uploads > 0 => format!("{} pending", s.pending_uploads),
            Some(_) if !m.spec.read_only => "synced".to_string(),
            _ => "-".to_string(),
        };
        let host = match h.host.kind {
            crate::manager::protocol::HostKind::Supervisor => format!("background (pid {})", h.host.pid),
            crate::manager::protocol::HostKind::Foreground => format!("foreground (pid {})", h.host.pid),
        };
        println!(
            "{:<7}{:<10}{:<7}{:<52}{:<10}{host}",
            m.spec.target,
            state_label(m.state),
            mode,
            truncate(&source, 50),
            uploads
        );
        if let Some(err) = &m.error {
            println!("       error: {err}");
        }
    }
    Ok(0)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

fn read_secret(prompt: &str, from_stdin: bool) -> Result<String, ErrorInfo> {
    let mut line = String::new();
    if from_stdin || !std::io::stdin().is_terminal() {
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| invalid(format!("could not read the token: {e}")))?;
        return Ok(line.trim().to_string());
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let _echo = super::console::EchoOff::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| invalid(format!("could not read the token: {e}")))?;
    eprintln!();
    Ok(line.trim().to_string())
}

fn auth_cmd(out: &Out, cmd: AuthCommand) -> Result<i32, ErrorInfo> {
    match cmd {
        AuthCommand::Login {
            token_stdin,
            no_validate,
            hub_endpoint,
        } => {
            let endpoint = endpoint_or_default(hub_endpoint.as_deref());
            let token = read_secret("Hugging Face token (input hidden): ", token_stdin)?;
            let token = Secret::new(token).ok_or_else(|| invalid("no token given"))?;
            let account = if no_validate {
                None
            } else {
                Some(runtime().block_on(auth::whoami(&endpoint, token.expose()))?)
            };
            credman::store(&endpoint, token.expose(), account.as_ref().map(|a| a.name.as_str()))
                .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("Credential Manager: {e}")))?;
            if out.json {
                out.json(json!({"ok": true, "account": account, "stored_in": "Windows Credential Manager"}));
            } else {
                match &account {
                    Some(a) => println!(
                        "Signed in as {}{}. Token stored in Windows Credential Manager.",
                        a.name,
                        a.token_role
                            .as_ref()
                            .map(|r| format!(" (token role: {r})"))
                            .unwrap_or_default()
                    ),
                    None => println!("Token stored in Windows Credential Manager (not validated)."),
                }
            }
            Ok(0)
        }
        AuthCommand::Status { offline, hub_endpoint } => {
            let endpoint = endpoint_or_default(hub_endpoint.as_deref());
            let stored = credman::read(&endpoint)
                .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("Credential Manager: {e}")))?;
            let env = ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]
                .into_iter()
                .find(|v| std::env::var(v).is_ok_and(|t| !t.trim().is_empty()));
            let cli_token = auth::hf_cli_token_path().filter(|p| p.is_file());
            let mut validation: Option<Result<auth::Account, ErrorInfo>> = None;
            if let Some((secret, _)) = &stored
                && !offline
            {
                validation = Some(runtime().block_on(auth::whoami(&endpoint, secret.expose())));
            }
            let (state, account) = match (&stored, &validation) {
                (None, _) => ("signed_out", None),
                (Some(_), None) => ("stored", None),
                (Some(_), Some(Ok(a))) => ("signed_in", Some(a.clone())),
                (Some(_), Some(Err(e))) if e.code == ErrorCode::InvalidToken => ("invalid_token", None),
                (Some(_), Some(Err(_))) => ("unverified", None),
            };
            if out.json {
                out.json(json!({
                    "ok": true,
                    "state": state,
                    "account": account,
                    "stored_account": stored.as_ref().and_then(|(_, a)| a.clone()),
                    "environment_variable": env,
                    "hf_cli_token": cli_token,
                    "endpoint": endpoint,
                }));
            } else {
                match state {
                    "signed_out" => println!("No token stored. Public repositories work without one."),
                    "stored" => println!(
                        "A token is stored{} (not checked: --offline).",
                        stored
                            .as_ref()
                            .and_then(|(_, a)| a.as_ref())
                            .map(|a| format!(" for {a}"))
                            .unwrap_or_default()
                    ),
                    "signed_in" => {
                        let a = account.as_ref().expect("signed in");
                        println!(
                            "Signed in as {}{}.",
                            a.name,
                            a.token_role
                                .as_ref()
                                .map(|r| format!(" (token role: {r})"))
                                .unwrap_or_default()
                        );
                    }
                    "invalid_token" => println!("The stored token is invalid or expired. Run `hf-mount auth login`."),
                    _ => println!(
                        "A token is stored but could not be checked: {}",
                        validation
                            .and_then(|v| v.err())
                            .map(|e| e.to_string())
                            .unwrap_or_default()
                    ),
                }
                if let Some(var) = env {
                    println!("Note: {var} is set in this environment and takes precedence for `hf-mount start`.");
                }
                if let Some(p) = cli_token {
                    println!(
                        "A huggingface_hub token exists at {} (used as a fallback).",
                        p.display()
                    );
                }
            }
            Ok(0)
        }
        AuthCommand::Logout { hub_endpoint } => {
            let endpoint = endpoint_or_default(hub_endpoint.as_deref());
            let removed = credman::delete(&endpoint)
                .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("Credential Manager: {e}")))?;
            if out.json {
                out.json(json!({"ok": true, "removed": removed}));
            } else if removed {
                println!("Removed the stored token. Existing mounts keep the token they started with until stopped.");
            } else {
                println!("No token was stored.");
            }
            Ok(0)
        }
    }
}

fn doctor(out: &Out) -> Result<i32, ErrorInfo> {
    let api = winfsp::api();
    let identity = identity()?;
    let rt = runtime();
    let supervisor = rt.block_on(async {
        match Connection::open(&identity.supervisor_pipe(), &identity).await {
            Ok(mut conn) => match conn.call(&Request::List).await {
                Ok(Response::Mounts { host, mounts }) => Ok(Some((host, mounts.len()))),
                _ => Ok(None),
            },
            Err(ConnectError::NotRunning) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    });
    let endpoint = endpoint_or_default(None);
    let started = std::time::Instant::now();
    let hub = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        client
            .get(format!("{endpoint}/api/models/openai-community/gpt2"))
            .send()
            .await
            .map(|r| r.status().as_u16())
            .map_err(|e| e.to_string())
    });
    let latency_ms = started.elapsed().as_millis() as u64;
    let stored = credman::read(&endpoint).ok().flatten().map(|(_, account)| account);
    let free = drives::free_letters();
    let settings = config::load_settings().unwrap_or_default();
    let elevated = security::is_elevated();

    if out.json {
        out.json(json!({
            "ok": api.is_ok(),
            "version": env!("CARGO_PKG_VERSION"),
            "winfsp": match &api {
                Ok(a) => json!({"installed": true, "version": a.version_string(), "install_dir": a.install_dir}),
                Err(e) => json!({"installed": false, "error": e.to_string()}),
            },
            "supervisor": match &supervisor {
                Ok(Some((h, n))) => json!({"running": true, "pid": h.pid, "mounts": n, "started_at": h.started_at}),
                Ok(None) => json!({"running": false}),
                Err(e) => json!({"running": false, "error": e}),
            },
            "session": {"user_sid": identity.sid, "session_id": identity.session, "elevated": elevated},
            "free_drive_letters": free.iter().map(|c| format!("{c}:")).collect::<Vec<_>>(),
            "hub": match &hub { Ok(s) => json!({"reachable": true, "status": s, "latency_ms": latency_ms, "endpoint": endpoint}), Err(e) => json!({"reachable": false, "error": e, "endpoint": endpoint}) },
            "token": {"stored": stored.is_some(), "account": stored.flatten()},
            "paths": {"home": config::home_dir(), "logs": logs_dir(), "cache": settings.cache_root()},
        }));
        return Ok(if api.is_ok() {
            0
        } else {
            ErrorCode::DriverMissing.exit_code()
        });
    }

    println!("hf-mount {} (Windows edition)", env!("CARGO_PKG_VERSION"));
    match &api {
        Ok(a) => println!(
            "  WinFsp       installed{} — {}",
            a.version_string().map(|v| format!(", version {v}")).unwrap_or_default(),
            a.install_dir.display()
        ),
        Err(e) => {
            println!("  WinFsp       NOT INSTALLED — {e}");
            println!("               Install: winget install WinFsp.WinFsp");
            println!("               or download the official installer from https://winfsp.dev/rel/");
            println!("               (needs administrator approval once; no reboot normally required)");
        }
    }
    match &supervisor {
        Ok(Some((h, n))) => println!(
            "  Supervisor   running (pid {}, {n} mount(s), since {})",
            h.pid, h.started_at
        ),
        Ok(None) => println!("  Supervisor   not running (starts automatically with `hf-mount start`)"),
        Err(e) => println!("  Supervisor   unreachable: {e}"),
    }
    println!(
        "  Session      {} (session {}){}",
        identity.sid,
        identity.session,
        if elevated {
            ", ELEVATED — drives will not show in File Explorer"
        } else {
            ""
        }
    );
    println!(
        "  Free letters {}",
        if free.is_empty() {
            "none".to_string()
        } else {
            free.iter().map(|c| format!("{c}:")).collect::<Vec<_>>().join(" ")
        }
    );
    match &hub {
        Ok(status) => println!("  Hub          reachable ({endpoint}, HTTP {status}, {latency_ms} ms)"),
        Err(e) => println!("  Hub          UNREACHABLE ({endpoint}): {e}"),
    }
    match &stored {
        Some(Some(account)) => println!("  Token        stored for {account}"),
        Some(None) => println!("  Token        stored"),
        None => println!("  Token        none (public repositories only; `hf-mount auth login` to add one)"),
    }
    println!("  Logs         {}", logs_dir().display());
    println!("  Cache        {}", settings.cache_root().display());
    println!();
    println!("Native drives are provided by {WINFSP_NOTICE}.");
    Ok(if api.is_ok() {
        0
    } else {
        ErrorCode::DriverMissing.exit_code()
    })
}

fn drives_cmd(out: &Out) -> Result<i32, ErrorInfo> {
    let free: Vec<String> = drives::free_letters().into_iter().map(|c| format!("{c}:")).collect();
    if out.json {
        out.json(json!({"ok": true, "free": free}));
    } else if free.is_empty() {
        println!("No free drive letters between D: and Z:.");
    } else {
        println!("{}", free.join(" "));
    }
    Ok(0)
}

fn activity(out: &Out, limit: usize) -> Result<i32, ErrorInfo> {
    let identity = identity()?;
    let response = runtime().block_on(async {
        let mut conn = Connection::open(&identity.supervisor_pipe(), &identity)
            .await
            .map_err(ErrorInfo::from)?;
        conn.call(&Request::Activity { limit }).await
    });
    let events = match response {
        Ok(Response::Activity { events }) => events,
        Ok(other) => return Err(client::expect_error(other)),
        Err(e) if e.code == ErrorCode::HostUnavailable => Vec::new(),
        Err(e) => return Err(e),
    };
    if out.json {
        out.json(json!({"ok": true, "events": events}));
    } else if events.is_empty() {
        println!("No activity (the background supervisor is not running or has no events).");
    } else {
        for e in events {
            let level = match e.level {
                crate::manager::protocol::EventLevel::Info => "info ",
                crate::manager::protocol::EventLevel::Warning => "warn ",
                crate::manager::protocol::EventLevel::Error => "error",
            };
            println!(
                "{}  {level}  {}{}",
                e.time,
                e.target.map(|t| format!("{t} ")).unwrap_or_default(),
                e.message
            );
        }
    }
    Ok(0)
}

fn retry_uploads(out: &Out, target: &str) -> Result<i32, ErrorInfo> {
    let target = normalize_target(target).map_err(invalid)?;
    let identity = identity()?;
    let rt = runtime();
    let (_, mut conn) = rt
        .block_on(client::host_for_target(&identity, &target))
        .ok_or_else(|| ErrorInfo::new(ErrorCode::NotMounted, format!("nothing is mounted at {target}")))?;
    match rt.block_on(conn.call(&Request::RetryUploads { target: target.clone() }))? {
        Response::Ok => {
            if out.json {
                out.json(json!({"ok": true}));
            } else {
                println!("Re-queued pending uploads on {target}.");
            }
            Ok(0)
        }
        other => Err(client::expect_error(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn upstream_syntax_parses() {
        let cli = Cli::try_parse_from(["hf-mount", "start", "repo", "openai-community/gpt2", "H:"]).unwrap();
        let Command::Start(args) = cli.command else { panic!() };
        let spec = build_spec(&args).unwrap();
        assert_eq!(spec.target, "H:");
        assert!(spec.read_only);

        let cli = Cli::try_parse_from([
            "hf-mount",
            "start",
            "repo",
            "datasets/username/dataset-name",
            "D:",
            "--revision",
            "main",
        ])
        .unwrap();
        let Command::Start(args) = cli.command else { panic!() };
        assert_eq!(build_spec(&args).unwrap().revision.as_deref(), Some("main"));

        let cli = Cli::try_parse_from([
            "hf-mount",
            "--json",
            "start",
            "bucket",
            "user/my-bucket",
            "X:",
            "--read-only",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Start(args) = cli.command else { panic!() };
        assert!(build_spec(&args).unwrap().read_only);

        assert!(Cli::try_parse_from(["hf-mount", "stop", "H:"]).is_ok());
        assert!(Cli::try_parse_from(["hf-mount", "stop", "--all"]).is_ok());
        assert!(Cli::try_parse_from(["hf-mount", "stop"]).is_err());
        assert!(Cli::try_parse_from(["hf-mount", "status", "--json"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "hf-mount",
                "start",
                "repo",
                "a/b",
                "H:",
                "--anonymous",
                "--hf-token",
                "x"
            ])
            .is_err()
        );
    }

    #[test]
    fn subfolder_twice_is_rejected() {
        let cli = Cli::try_parse_from([
            "hf-mount",
            "start",
            "repo",
            "user/model/sub",
            "H:",
            "--subfolder",
            "other",
        ])
        .unwrap();
        let Command::Start(args) = cli.command else { panic!() };
        assert_eq!(build_spec(&args).unwrap_err().code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("500MB").unwrap(), 500_000_000);
        assert_eq!(parse_size("20GB").unwrap(), 20_000_000_000);
        assert_eq!(parse_size("1GiB").unwrap(), 1 << 30);
        assert_eq!(parse_size("1234").unwrap(), 1234);
        assert!(parse_size("ten").is_err());
        assert!(parse_size("5XB").is_err());
    }
}
