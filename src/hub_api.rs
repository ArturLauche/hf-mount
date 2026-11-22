use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};
use xet_client::cas_client::auth::{AuthError, TokenInfo, TokenRefresher};

use crate::error::{Error, Result, is_retryable_status, is_transient_http};

mod tree;
pub use tree::TreeEntry;

/// Characters that must be percent-encoded when a file path is interpolated
/// into a URL path: `#` starts a fragment, `?` a query string, `%` corrupts
/// decoding, and the rest are not valid in URL paths. `/` is intentionally
/// kept so multi-segment paths stay multi-segment.
const URL_PATH_ESCAPE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'?')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^');

/// Percent-encode a file path for use in a URL path, preserving `/`.
/// Borrows (no allocation) when nothing needs escaping — the common case.
///
/// Filenames with URL-reserved characters are routine: object_store-based
/// writers (Lance, Delta) stage uploads as `name#1`, `name#2`, … — without
/// encoding, everything after `#` is parsed as a fragment and every such
/// file resolves to the same truncated URL.
fn encode_url_path(path: &str) -> std::borrow::Cow<'_, str> {
    utf8_percent_encode(path, URL_PATH_ESCAPE).into()
}

/// Revisions escape `/` too: a ref like `refs/pr/1` occupies a single path
/// segment in `/resolve/{revision}/{path}` and `/tree/{revision}/{path}`, so
/// it must land as `refs%2Fpr%2F1` — unencoded, the Hub parses the extra
/// segments as part of the file path and returns 404.
const URL_REVISION_ESCAPE: &AsciiSet = &URL_PATH_ESCAPE.add(b'/');

/// Percent-encode a revision as a single URL path segment (see
/// `URL_REVISION_ESCAPE`).
fn encode_url_revision(revision: &str) -> std::borrow::Cow<'_, str> {
    utf8_percent_encode(revision, URL_REVISION_ESCAPE).into()
}

/// Startup and polling must inspect the same ref as tree and resolve requests.
/// Request only the metadata they use; a full repo info response includes its file list.
fn repo_info_url(endpoint: &str, repo_type: RepoType, repo_id: &str, revision: &str) -> String {
    format!(
        "{endpoint}/api/{}/{repo_id}/revision/{}?expand=sha&expand=lastModified",
        repo_type.api_prefix(),
        encode_url_revision(revision)
    )
}

/// The path of `path` relative to the directory `prefix`, if `path` lives
/// strictly under `{prefix}/` — as opposed to merely sharing `prefix` as a
/// raw key prefix (`a/1.manifest#1` vs `a/1.manifest`) or being `prefix`
/// itself.
pub(crate) fn strict_descendant_rel<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rel| !rel.is_empty())
}

// ── HubOps trait ──────────────────────────────────────────────────────

/// Trait abstracting the Hub API operations used by VirtualFs and FlushManager.
/// Production code uses `HubApiClient`; tests inject mocks.
#[async_trait::async_trait]
pub trait HubOps: Send + Sync {
    async fn list_tree(&self, prefix: &str) -> Result<Vec<TreeEntry>>;
    /// Does the directory have at least one strict descendant? Implementations
    /// with paginated listings can stop as soon as they find a child.
    async fn has_children(&self, prefix: &str) -> Result<bool> {
        Ok(!self.list_tree(prefix).await?.is_empty())
    }
    async fn head_file(&self, path: &str) -> Result<Option<HeadFileInfo>>;
    async fn batch_operations(&self, ops: &[BatchOp]) -> Result<()>;
    async fn download_file_http(&self, path: &str, dest: &Path) -> Result<()>;
    fn default_mtime(&self) -> SystemTime;
    fn source(&self) -> &SourceKind;
    fn is_repo(&self) -> bool;

    /// Cheap probe returning an opaque "revision" token that changes whenever
    /// the source content changes. For repos this is the commit head SHA; for
    /// buckets it is `updatedAt`. Bucket polling and live-follow use this
    /// token. Repo polling needs an immutable SHA from `probe_repo_commit`.
    ///
    /// Errors fall through to a full fan-out — the probe is an optimization,
    /// not a gate. The default impl errors out so mocks that don't care about
    /// the probe path keep doing full polls.
    async fn probe_revision(&self) -> Result<String> {
        Err(Error::hub("probe_revision not implemented"))
    }

    /// Resolve the mounted repo ref to an immutable commit that
    /// `list_tree_at_commit` can read. Without one, poll the mounted ref
    /// normally and leave the round uncheckpointed.
    async fn probe_repo_commit(&self) -> Result<Option<String>> {
        Ok(None)
    }

    /// List a directory at a commit returned by `probe_repo_commit`.
    async fn list_tree_at_commit(&self, _prefix: &str, _commit: &str) -> Result<Vec<TreeEntry>> {
        Err(Error::hub("commit-pinned tree listing unavailable"))
    }

    /// Open the bucket live-follow SSE stream (`GET /api/buckets/{id}/events`).
    ///
    /// `cursor` resumes strictly after a previously received opaque cursor;
    /// `since` (ISO8601, e.g. the last `updatedAt` seen) replays changes from
    /// that instant; with neither, only live changes stream (callers pass at
    /// most one). `Ok(None)` means this source or deployment doesn't serve
    /// the feed (repo source, 404, non-SSE response): the caller falls back
    /// to polling permanently. Errors carry the HTTP status (a 400 means the
    /// request itself was refused, e.g. an unparseable resume point) and, on
    /// 503/429, the server's wait hint via [`Error::retry_after`]. The
    /// default impl reports the feed as absent so mocks and non-bucket
    /// sources use the poll fallback.
    async fn follow_events(
        &self,
        _cursor: Option<&str>,
        _since: Option<&str>,
    ) -> Result<Option<Box<dyn crate::follow::FollowStreamOps>>> {
        Ok(None)
    }
}

// ── Repo / Bucket types ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RepoType {
    Model,
    Dataset,
    Space,
}

impl RepoType {
    /// Path segment used in `/api/{type}/{id}/…` API routes.
    pub fn api_prefix(&self) -> &'static str {
        match self {
            Self::Model => "models",
            Self::Dataset => "datasets",
            Self::Space => "spaces",
        }
    }

    /// Path segment used in `/{type}/{id}/resolve/…` user-facing routes.
    pub fn resolve_prefix(&self) -> &'static str {
        // Models don't have a type prefix in resolve URLs (e.g. /user/repo/resolve/main/file)
        // Datasets and spaces do (e.g. /datasets/user/repo/resolve/main/file)
        match self {
            Self::Model => "",
            Self::Dataset => "datasets/",
            Self::Space => "spaces/",
        }
    }
}

impl std::str::FromStr for RepoType {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "model" => Ok(Self::Model),
            "dataset" => Ok(Self::Dataset),
            "space" => Ok(Self::Space),
            _ => Err(format!("unknown repo type: {s} (expected model, dataset, or space)")),
        }
    }
}

impl std::fmt::Display for RepoType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model => f.write_str("model"),
            Self::Dataset => f.write_str("dataset"),
            Self::Space => f.write_str("space"),
        }
    }
}

/// Identifies whether we're talking to a bucket or a repo.
/// Also serves as the clap subcommand for the CLI.
#[derive(Debug, Clone)]
pub enum SourceKind {
    Bucket {
        bucket_id: String,
    },
    Repo {
        repo_id: String,
        repo_type: RepoType,
        revision: String,
    },
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bucket { bucket_id } => write!(f, "bucket/{bucket_id}"),
            Self::Repo {
                repo_id,
                repo_type,
                revision,
            } => write!(f, "{repo_type}/{repo_id}/{revision}"),
        }
    }
}

// ── Shared data types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BatchOp {
    #[serde(rename_all = "camelCase")]
    AddFile {
        path: String,
        xet_hash: String,
        mtime: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    DeleteFile { path: String },
}

/// Metadata returned by HEAD on the resolve endpoint
#[derive(Debug)]
pub struct HeadFileInfo {
    pub xet_hash: Option<String>,
    pub etag: Option<String>,
    pub size: Option<u64>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CasTokenInfo {
    pub cas_url: String,
    pub exp: u64,
    pub access_token: String,
}

// ── HubApiClient ──────────────────────────────────────────────────────

/// How often the token file is re-read from disk.
const TOKEN_FILE_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);

// Give older Hub deployments a break between optional commit-history requests.
const COMMIT_HISTORY_RETRY_POLLS: u8 = 8;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CommitHistory {
    #[default]
    Unknown,
    Available,
    Unavailable {
        polls_left: u8,
    },
}

impl CommitHistory {
    fn next_poll(&mut self) -> Self {
        let current = *self;
        if let Self::Unavailable { polls_left } = current {
            *self = if polls_left <= 1 {
                Self::Unknown
            } else {
                Self::Unavailable {
                    polls_left: polls_left - 1,
                }
            };
        }
        current
    }
}

pub struct HubApiClient {
    client: Client,
    /// Client that does NOT follow redirects — used for HEAD requests where we
    /// need response headers from the Hub (not from the CAS redirect target).
    head_client: Client,
    /// Client for the live-follow SSE stream: no whole-request timeout (the
    /// stream is long-lived by design), a per-read timeout instead. The
    /// server pings every 30s, so 90s of silence means a dead connection.
    follow_client: Client,
    endpoint: String,
    token: Option<String>,
    /// Path to a file containing the API token. Re-read periodically so
    /// the CSI driver can refresh credentials without remounting.
    /// Takes precedence over `token` when the file exists and is non-empty.
    token_file: Option<PathBuf>,
    /// Cached token from `token_file`, refreshed every 30s.
    token_file_cache: std::sync::Mutex<Option<(std::time::Instant, String)>>,
    source: SourceKind,
    /// Some older Hub deployments omit sha from revision metadata. Remember
    /// whether the commit-history endpoint can supply the mounted ref's head.
    commit_history: Mutex<CommitHistory>,
    /// Last modification time (from repo/bucket info endpoint).
    /// Used as default mtime when per-file mtime is unavailable.
    last_modified: SystemTime,
    /// Optional subfolder prefix. When non-empty, all API calls transparently
    /// prepend this to outgoing paths and strip it from incoming TreeEntry paths.
    path_prefix: String,
}

/// Parse a repo ID, extracting the type from an optional prefix.
/// "datasets/user/ds" → (Dataset, "user/ds")
/// "spaces/user/app" → (Space, "user/app")
/// "user/model" → (Model, "user/model")
pub fn parse_repo_id(repo_id: &str) -> (RepoType, String) {
    if let Some(rest) = repo_id.strip_prefix("datasets/") {
        (RepoType::Dataset, rest.to_string())
    } else if let Some(rest) = repo_id.strip_prefix("spaces/") {
        (RepoType::Space, rest.to_string())
    } else {
        (RepoType::Model, repo_id.to_string())
    }
}

/// Split a raw identifier into `(id, path_prefix)` after the first 2 `/`-separated segments.
/// E.g. `split_path_prefix("user/bucket/a/b")` → `("user/bucket", "a/b")`.
/// Trailing slashes are trimmed, and `.`/`..` components in the prefix are rejected.
pub fn split_path_prefix(raw: &str) -> std::result::Result<(&str, &str), &'static str> {
    let raw = raw.trim_end_matches('/');
    let mut end = 0;
    let mut count = 0;
    for (i, ch) in raw.char_indices() {
        if ch == '/' {
            count += 1;
            if count == 2 {
                end = i;
                break;
            }
        }
    }
    if count < 2 {
        // Not enough segments — entire string is the ID, no prefix.
        Ok((raw, ""))
    } else {
        let prefix = &raw[end + 1..];
        if prefix.split('/').any(|s| s == "." || s == "..") {
            return Err("path prefix must not contain '.' or '..' components");
        }
        Ok((&raw[..end], prefix))
    }
}

pub(crate) fn retry_delay(attempt: u32) -> std::time::Duration {
    debug_assert!(attempt > 0, "retry_delay called with attempt=0");
    // Saturating: callers cap the result (MAX_RETRY_DELAY), but a long
    // startup retry loop can push `attempt` past 63 and 2^attempt would
    // overflow before the cap applies.
    std::time::Duration::from_millis(500u64.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1))))
}

/// Upper bound on a single in-request retry sleep, whether server-hinted
/// (RateLimit / Retry-After header) or exponential.
pub(crate) const MAX_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// A response header as a string, when present and ASCII.
fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Server-requested wait before retrying, uncapped (each retry loop applies
/// its own ceiling): the IETF `RateLimit` header's `t=<seconds>` (time until
/// window reset, what moon-landing sends on 429; format
/// `"resource_type";r=<remaining>;t=<seconds_until_reset>`), else a
/// `Retry-After: <seconds>` header (what the live-follow endpoint sends on
/// 503). A zero hint means the window already reset: no hint, use the
/// backoff schedule rather than retrying immediately.
fn parse_retry_delay(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let ratelimit_secs = header_str(headers, "ratelimit").and_then(|value| {
        value
            .split(';')
            .find_map(|part| part.trim().strip_prefix("t=")?.parse::<u64>().ok())
    });
    let secs = ratelimit_secs.or_else(|| header_str(headers, "retry-after")?.trim().parse::<u64>().ok())?;
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Build an authenticated GET request during client initialization (before HubApiClient exists).
fn init_auth_get(
    client: &Client,
    url: &str,
    token: Option<&str>,
    token_file: &Option<PathBuf>,
) -> reqwest::RequestBuilder {
    let file_token = if token.is_none() {
        token_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let effective_token = token.or(file_token.as_deref());
    match effective_token {
        Some(t) => client.get(url).bearer_auth(t),
        None => client.get(url),
    }
}

/// Best-effort probe: does `id` resolve as a repo (model, dataset, or space)?
/// Returns the matching repo type if a 200 response is seen. Any error or
/// non-200 status is treated as "not this kind" — used only to enrich error
/// messages, never on a hot path.
async fn probe_repo(
    client: &Client,
    endpoint: &str,
    id: &str,
    token: Option<&str>,
    token_file: &Option<PathBuf>,
) -> Option<RepoType> {
    for repo_type in [RepoType::Model, RepoType::Dataset, RepoType::Space] {
        let url = format!("{endpoint}/api/{}/{id}", repo_type.api_prefix());
        let Ok(resp) = init_auth_get(client, &url, token, token_file).send().await else {
            continue;
        };
        if resp.status().is_success() {
            return Some(repo_type);
        }
    }
    None
}

/// Send an HTTP request with automatic retry on transient errors (408, 429, 5xx, timeouts).
/// Uses the IETF RateLimit header's t= parameter when present, falls back to exponential backoff (2 retries max).
/// When the retries are exhausted the returned error carries the server's last
/// RateLimit hint (`Error::retry_after`) so an outer loop can keep honoring it.
/// Set `accept_redirects` to treat 3xx as success (needed for HEAD on /resolve/ endpoints
/// where the redirect response itself carries metadata headers).
async fn send_with_retry(
    build_request: impl Fn() -> reqwest::RequestBuilder,
    context: &str,
    accept_redirects: bool,
) -> Result<reqwest::Response> {
    const MAX_RETRIES: u32 = 2;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match build_request().send().await {
            Ok(resp) if resp.status().is_success() || (accept_redirects && resp.status().is_redirection()) => {
                return Ok(resp);
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                let hinted_delay = parse_retry_delay(resp.headers());
                if is_retryable_status(status) && attempt <= MAX_RETRIES {
                    let delay = hinted_delay
                        .unwrap_or_else(|| retry_delay(attempt))
                        .min(MAX_RETRY_DELAY);
                    warn!("{context}: transient error ({status}), retry {attempt}/{MAX_RETRIES} in {delay:?}");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::Hub {
                    message: format!("{context}: {status} {body}"),
                    status: Some(status),
                    retry_after: hinted_delay,
                });
            }
            Err(err) if is_transient_http(&err) && attempt <= MAX_RETRIES => {
                let delay = retry_delay(attempt);
                warn!("{context}: transient error, retry {attempt}/{MAX_RETRIES} in {delay:?}: {err}");
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(Error::Http(err)),
        }
    }
}

fn make_clients(backend: &str) -> (Client, Client, Client) {
    let user_agent = format!("hf-mount/{}; fs/{}", env!("CARGO_PKG_VERSION"), backend);
    // Idle pool / keep-alive shared across the clients so a hung Hub doesn't
    // freeze the poll loop and TLS handshakes are amortized across rounds.
    let base = || {
        reqwest::Client::builder()
            .user_agent(&user_agent)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .connect_timeout(Duration::from_secs(10))
    };
    let client = base()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("failed to build client");
    let head_client = base()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build head_client");
    // See HubApiClient::follow_client: a whole-request timeout would cut the
    // SSE stream mid-session, so bound each body read instead — the server's
    // 30s pings keep a healthy connection under the 90s ceiling.
    let follow_client = base()
        .read_timeout(Duration::from_secs(90))
        .build()
        .expect("failed to build follow_client");
    (client, head_client, follow_client)
}

impl HubApiClient {
    /// Create a client for a bucket or repo. Resolve repo aliases (e.g.
    /// "gpt2" → "openai-community/gpt2") and fetch the mounted revision's mtime.
    pub async fn from_source(
        endpoint: &str,
        token: Option<&str>,
        token_file: Option<PathBuf>,
        source: SourceKind,
        path_prefix: String,
        backend: &str,
    ) -> Result<Arc<Self>> {
        let (client, head_client, follow_client) = make_clients(backend);
        let endpoint = endpoint.trim_end_matches('/').to_string();

        let (source, last_modified) = match source {
            SourceKind::Repo {
                repo_id,
                repo_type,
                revision,
            } => {
                let url = repo_info_url(&endpoint, repo_type, &repo_id, &revision);
                let context = format!("resolve repo {repo_id}");
                let resp =
                    send_with_retry(|| init_auth_get(&client, &url, token, &token_file), &context, false).await?;
                let body: serde_json::Value = resp.json().await?;
                let resolved_id = body["id"]
                    .as_str()
                    .ok_or_else(|| Error::hub("repo info missing 'id' field"))?;
                if resolved_id != repo_id {
                    info!("Resolved repo alias: {} → {}", repo_id, resolved_id);
                }
                let last_modified = body["lastModified"].as_str().map(mtime_from_str).unwrap_or(UNIX_EPOCH);
                (
                    SourceKind::Repo {
                        repo_id: resolved_id.to_string(),
                        repo_type,
                        revision,
                    },
                    last_modified,
                )
            }
            SourceKind::Bucket { bucket_id } => {
                let url = format!("{}/api/buckets/{}", endpoint, bucket_id);
                let context = format!("resolve bucket {bucket_id}");
                let resp =
                    match send_with_retry(|| init_auth_get(&client, &url, token, &token_file), &context, false).await {
                        Ok(r) => r,
                        Err(err) => {
                            // Common mistake: user passed a repo id to `bucket`. Probe the
                            // repo APIs and, if one matches, surface a hint instead of the
                            // raw 401 from the bucket endpoint. Skip the probe on transient
                            // failures (rate limit, 5xx): it cannot succeed while the user
                            // is being throttled, and startup retries would multiply its 3
                            // extra requests into the very storm being waited out.
                            if !err.is_transient()
                                && let Some(repo_type) =
                                    probe_repo(&client, &endpoint, &bucket_id, token, &token_file).await
                            {
                                return Err(Error::hub(format!(
                                    "{bucket_id} is not a bucket, but it exists as a {repo_type}. \
                                 Use `repo {bucket_id}` (read-only) instead of `bucket {bucket_id}`."
                                )));
                            }
                            return Err(err);
                        }
                    };
                let body: serde_json::Value = resp.json().await?;
                let last_modified = body["updatedAt"].as_str().map(mtime_from_str).unwrap_or(UNIX_EPOCH);
                (SourceKind::Bucket { bucket_id }, last_modified)
            }
        };

        Ok(Arc::new(Self {
            commit_history: Mutex::default(),
            client,
            head_client,
            follow_client,
            endpoint,
            token: token.map(|t| t.to_string()),
            token_file,
            token_file_cache: std::sync::Mutex::new(None),
            source,
            last_modified,
            path_prefix,
        }))
    }

    /// Create a client for a HuggingFace bucket.
    pub fn new(endpoint: &str, token: Option<&str>, bucket_id: &str, backend: &str) -> Arc<Self> {
        let (client, head_client, follow_client) = make_clients(backend);
        Arc::new(Self {
            commit_history: Mutex::default(),
            client,
            head_client,
            follow_client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            token: token.map(|t| t.to_string()),
            token_file: None,
            token_file_cache: std::sync::Mutex::new(None),
            source: SourceKind::Bucket {
                bucket_id: bucket_id.to_string(),
            },
            last_modified: UNIX_EPOCH,
            path_prefix: String::new(),
        })
    }

    /// Attach bearer auth to a request if a token is configured.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(path) = &self.token_file {
            // Check TTL under lock, but do file I/O outside it to avoid
            // blocking concurrent requests on slow storage.
            let need_refresh = {
                let cache = self.token_file_cache.lock().expect("token_file_cache poisoned");
                match &*cache {
                    Some((at, _)) => at.elapsed() >= TOKEN_FILE_REFRESH,
                    None => true,
                }
            };
            if need_refresh && let Ok(contents) = std::fs::read_to_string(path) {
                let token = contents.trim().to_string();
                if !token.is_empty() {
                    let mut cache = self.token_file_cache.lock().expect("token_file_cache poisoned");
                    *cache = Some((std::time::Instant::now(), token));
                }
            }
            let cache = self.token_file_cache.lock().expect("token_file_cache poisoned");
            if let Some((_, ref token)) = *cache {
                return req.bearer_auth(token);
            }
        }
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    pub fn source(&self) -> &SourceKind {
        &self.source
    }

    /// Default mtime when per-file mtime is unavailable.
    pub fn default_mtime(&self) -> SystemTime {
        self.last_modified
    }

    pub fn is_repo(&self) -> bool {
        matches!(self.source, SourceKind::Repo { .. })
    }

    /// Return the path prefix (subfolder) this client is scoped to.
    pub fn path_prefix(&self) -> &str {
        &self.path_prefix
    }

    /// Join `path_prefix` and `path` into the full API path.
    fn prefixed_path(&self, path: &str) -> String {
        if self.path_prefix.is_empty() {
            path.to_string()
        } else if path.is_empty() {
            self.path_prefix.clone()
        } else {
            format!("{}/{}", self.path_prefix, path)
        }
    }

    /// Validate that the path prefix exists on the remote.
    /// Probe for a child without enumerating the whole subfolder twice at mount.
    pub async fn validate_path_prefix(&self) -> Result<()> {
        if self.path_prefix.is_empty() {
            return Ok(());
        }
        if !self.has_children("").await? {
            return Err(Error::hub(format!(
                "subfolder '{}' is empty or does not exist",
                self.path_prefix
            )));
        }
        Ok(())
    }

    /// Fetch metadata for the mounted repo revision or bucket and return an
    /// opaque revision token. Older repo endpoints can return a timestamp
    /// when a commit SHA is unavailable.
    ///
    /// Repos: prefer `sha` (commit head) — changes on every push and only on
    /// pushes. Buckets: `updatedAt` — set by Hub on every bucket mutation.
    /// Errors if the Hub response omits the expected field (should not happen
    /// against the production Hub).
    pub async fn probe_revision(&self) -> Result<String> {
        let probe = self.revision_metadata().await?;
        match &self.source {
            SourceKind::Repo { .. } => probe
                .sha
                .or(probe.last_modified)
                .ok_or_else(|| Error::hub("revision probe: repo response missing sha and lastModified")),
            SourceKind::Bucket { .. } => probe
                .updated_at
                .ok_or_else(|| Error::hub("revision probe: bucket response missing updatedAt")),
        }
    }

    async fn revision_metadata(&self) -> Result<RevisionProbe> {
        let url = match &self.source {
            SourceKind::Repo {
                repo_id,
                repo_type,
                revision,
            } => repo_info_url(&self.endpoint, *repo_type, repo_id, revision),
            SourceKind::Bucket { bucket_id } => {
                format!("{}/api/buckets/{}", self.endpoint, bucket_id)
            }
        };
        let resp = send_with_retry(|| self.auth(self.client.get(&url)), "revision probe", false).await?;
        Ok(resp.json().await?)
    }

    /// Optional fallback for timestamp-only revision metadata. Request just the
    /// newest commit on the selected ref. An unavailable history never prevents
    /// the poller from reconciling the mutable tree.
    async fn history_head(&self, repo_type: RepoType, repo_id: &str, revision: &str) -> Option<String> {
        #[derive(Deserialize)]
        struct Commit {
            id: String,
        }

        let url = format!(
            "{}/api/{}/{repo_id}/commits/{}?limit=1",
            self.endpoint,
            repo_type.api_prefix(),
            encode_url_revision(revision)
        );
        let result: Result<Option<String>> = async {
            let response = self.auth(self.client.get(url)).send().await?;
            if matches!(response.status().as_u16(), 404 | 405 | 501) {
                *self.commit_history.lock().expect("commit history poisoned") = CommitHistory::Unavailable {
                    polls_left: COMMIT_HISTORY_RETRY_POLLS,
                };
                return Ok(None);
            }
            let commits: Vec<Commit> = response.error_for_status()?.json().await?;
            let head = commits
                .into_iter()
                .next()
                .map(|c| c.id)
                .filter(|id| matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit()));
            if head.is_some() {
                *self.commit_history.lock().expect("commit history poisoned") = CommitHistory::Available;
            }
            Ok(head)
        }
        .await;
        result.unwrap_or_else(|e| {
            debug!("commit history probe unavailable for {repo_id}@{revision}: {e}");
            None
        })
    }

    /// Open the bucket live-follow SSE stream. See [`HubOps::follow_events`]
    /// for the contract; this is the transport: `GET
    /// /api/buckets/{id}/events` with `Accept: text/event-stream` (mandatory
    /// — the server 400s without it) and the same bearer auth as tree calls.
    /// No `send_with_retry`: a 503 here has dedicated semantics (this pod
    /// doesn't serve the feed or is catching up; its `Retry-After` is
    /// surfaced through [`Error::retry_after`]) and everything else is
    /// handled by the caller's reconnect/backoff loop.
    pub async fn follow_events(
        &self,
        cursor: Option<&str>,
        since: Option<&str>,
    ) -> Result<Option<Box<dyn crate::follow::FollowStreamOps>>> {
        let SourceKind::Bucket { bucket_id } = &self.source else {
            return Ok(None); // repos have no live-follow feed
        };
        let url = format!("{}/api/buckets/{}/events", self.endpoint, bucket_id);
        let mut req = self
            .auth(self.follow_client.get(&url))
            .header("accept", "text/event-stream");
        if let Some(cursor) = cursor {
            req = req.query(&[("cursor", cursor)]);
        } else if let Some(since) = since {
            req = req.query(&[("since", since)]);
        }
        let resp = req.send().await.map_err(Error::Http)?;
        let status = resp.status().as_u16();
        if status == 404 {
            debug!("live-follow: endpoint not served by this Hub (404)");
            return Ok(None);
        }
        if !resp.status().is_success() {
            let retry_after = parse_retry_delay(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Hub {
                message: format!("live-follow: {status} {body}"),
                status: Some(status),
                retry_after,
            });
        }
        // A 200 that isn't an event stream (a proxy or intermediate Hub
        // build answering with HTML/JSON) would never yield an event: treat
        // it as "feed not served" rather than reconnecting forever.
        let content_type = header_str(resp.headers(), "content-type").unwrap_or_default();
        if !content_type.starts_with("text/event-stream") {
            warn!("live-follow: endpoint answered with content-type {content_type:?}, not an event stream");
            return Ok(None);
        }
        Ok(Some(Box::new(crate::follow::HttpFollowStream::new(
            resp,
            self.path_prefix.clone(),
        ))))
    }

    /// Build the resolve-endpoint URL for `path` (prefixed and URL-encoded).
    fn resolve_url(&self, path: &str) -> String {
        let prefixed = self.prefixed_path(path);
        let api_path = encode_url_path(&prefixed);
        match &self.source {
            // Buckets: /buckets/{id}/resolve/{path} (no /api/ prefix)
            SourceKind::Bucket { bucket_id } => {
                format!("{}/buckets/{}/resolve/{}", self.endpoint, bucket_id, api_path)
            }
            // Repos: /{resolve_prefix}{id}/resolve/{revision}/{path}
            SourceKind::Repo {
                repo_id,
                repo_type,
                revision,
            } => {
                format!(
                    "{}/{}{}/resolve/{}/{}",
                    self.endpoint,
                    repo_type.resolve_prefix(),
                    repo_id,
                    encode_url_revision(revision),
                    api_path,
                )
            }
        }
    }

    /// Fetch metadata for a single file via HEAD on the resolve endpoint.
    /// Returns `None` if 404 (file does not exist remotely).
    pub async fn head_file(&self, path: &str) -> Result<Option<HeadFileInfo>> {
        let url = self.resolve_url(path);
        let resp = send_with_retry(|| self.auth(self.head_client.head(&url)), "head_file", true).await;
        let resp = match resp {
            Ok(r) => r,
            Err(Error::Hub { status: Some(404), .. }) => return Ok(None),
            Err(err) => return Err(err),
        };

        let redirected = resp.status().is_redirection();
        let headers = resp.headers();
        let xet_hash = headers
            .get("x-xet-hash")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        // On a redirect (LFS/Xet files to the CDN, plain files of public repos
        // to /api/resolve-cache), content-length and etag describe the redirect
        // body, not the file: only the x-linked-* headers apply. Without
        // x-linked-size the size stays unknown and callers fall back to the
        // tree listing. A 200 (private or gated repos) serves the file itself.
        let size = headers
            .get("x-linked-size")
            .or_else(|| headers.get("content-length").filter(|_| !redirected))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let etag = headers
            .get("x-linked-etag")
            .or_else(|| headers.get("etag").filter(|_| !redirected))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());
        let last_modified = headers
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        Ok(Some(HeadFileInfo {
            xet_hash,
            etag,
            size,
            last_modified,
        }))
    }

    /// Get a CAS read token.
    pub async fn get_cas_token(&self) -> Result<CasTokenInfo> {
        let url = match &self.source {
            SourceKind::Bucket { bucket_id } => {
                format!("{}/api/buckets/{}/xet-read-token", self.endpoint, bucket_id)
            }
            SourceKind::Repo {
                repo_id,
                repo_type,
                revision,
            } => {
                format!(
                    "{}/api/{}/{}/xet-read-token/{}",
                    self.endpoint,
                    repo_type.api_prefix(),
                    repo_id,
                    encode_url_revision(revision),
                )
            }
        };

        let resp = send_with_retry(|| self.auth(self.client.get(&url)), "CAS token request", false).await?;
        let info: CasTokenInfo = resp.json().await?;
        Ok(info)
    }

    /// Get a CAS write token (buckets only).
    pub async fn get_cas_write_token(&self) -> Result<CasTokenInfo> {
        let bucket_id = match &self.source {
            SourceKind::Bucket { bucket_id } => bucket_id,
            SourceKind::Repo { .. } => {
                return Err(Error::hub("write tokens not supported for repos"));
            }
        };
        let url = format!("{}/api/buckets/{}/xet-write-token", self.endpoint, bucket_id);

        let resp = send_with_retry(|| self.auth(self.client.get(&url)), "CAS write token request", false).await?;
        let info: CasTokenInfo = resp.json().await?;
        Ok(info)
    }

    /// Execute batch operations (add/delete files) on the bucket.
    pub async fn batch_operations(&self, ops: &[BatchOp]) -> Result<()> {
        let bucket_id = match &self.source {
            SourceKind::Bucket { bucket_id } => bucket_id,
            SourceKind::Repo { .. } => {
                return Err(Error::hub("batch operations not supported for repos"));
            }
        };
        let url = format!("{}/api/buckets/{}/batch", self.endpoint, bucket_id);

        // Build NDJSON body, prepending path_prefix to each op's path.
        let mut body = String::new();
        for op in ops {
            if self.path_prefix.is_empty() {
                body.push_str(&serde_json::to_string(op)?);
            } else {
                let prefixed = match op {
                    BatchOp::AddFile {
                        path,
                        xet_hash,
                        mtime,
                        content_type,
                    } => BatchOp::AddFile {
                        path: self.prefixed_path(path),
                        xet_hash: xet_hash.clone(),
                        mtime: *mtime,
                        content_type: content_type.clone(),
                    },
                    BatchOp::DeleteFile { path } => BatchOp::DeleteFile {
                        path: self.prefixed_path(path),
                    },
                };
                body.push_str(&serde_json::to_string(&prefixed)?);
            }
            body.push('\n');
        }

        let body = bytes::Bytes::from(body);
        send_with_retry(
            || {
                self.auth(self.client.post(&url))
                    .header("content-type", "application/x-ndjson")
                    .body(body.clone())
            },
            "batch operation",
            false,
        )
        .await?;

        Ok(())
    }

    /// Download a file via HTTP GET on the resolve endpoint and write it to `dest`.
    /// Used for non-Xet files in repos (no xet hash).
    ///
    /// Supports ETag-based conditional requests: if `dest` already exists and a
    /// sidecar `{dest}.etag` file is present, sends `If-None-Match`. On 304 the
    /// existing cached file is kept as-is.
    pub async fn download_file_http(&self, path: &str, dest: &Path) -> Result<()> {
        let url = self.resolve_url(path);

        // Read cached ETag only if dest exists — otherwise an orphan sidecar
        // (e.g. user manually deleted dest) could trick us into a 304 and
        // `return Ok(())` without dest actually being on disk.
        let etag_path = dest.with_extension("etag");
        let cached_etag = if tokio::fs::try_exists(dest).await.unwrap_or(false) {
            tokio::fs::read_to_string(&etag_path).await.ok()
        } else {
            None
        };

        info!("HTTP download: {} → {:?}", path, dest);
        let resp = send_with_retry(
            || {
                let mut r = self.auth(self.client.get(&url));
                if let Some(ref etag) = cached_etag {
                    r = r.header("If-None-Match", format!("\"{}\"", etag.trim()));
                }
                r
            },
            "HTTP download",
            false,
        )
        .await;
        let resp = match resp {
            Ok(r) => r,
            Err(Error::Hub { status: Some(304), .. }) => {
                info!("HTTP cache hit (304): {}", path);
                return Ok(());
            }
            Err(err) => return Err(err),
        };

        let new_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());

        // Stream response body to a temp file, then atomic-rename to dest.
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = dest.with_extension(format!("tmp.{}", std::process::id()));
        let result: std::result::Result<(), Error> = async {
            let mut file = tokio::fs::File::create(&tmp).await?;
            let mut stream = resp.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                file.write_all(&chunk).await?;
            }
            file.shutdown().await?;
            drop(file);
            tokio::fs::rename(&tmp, dest).await?;
            if let Some(etag) = &new_etag {
                tokio::fs::write(&etag_path, etag).await.ok();
            } else {
                tokio::fs::remove_file(&etag_path).await.ok();
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            tokio::fs::remove_file(&tmp).await.ok();
        }
        result
    }

    /// Create a token refresher for this source.
    /// Uses a write token when `read_only` is false (write tokens can also read).
    pub fn token_refresher(self: &Arc<Self>, read_only: bool) -> Arc<HubTokenRefresher> {
        let kind = if read_only { TokenKind::Read } else { TokenKind::Write };
        Arc::new(HubTokenRefresher {
            hub_client: self.clone(),
            kind,
        })
    }
}

pub fn mtime_from_str(s: &str) -> SystemTime {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp()).ok())
        .map(|secs| UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap_or(UNIX_EPOCH)
}

/// Parse HTTP-date format (e.g. "Sat, 28 Feb 2026 14:52:39 GMT") from Last-Modified header.
pub fn mtime_from_http_date(s: &str) -> SystemTime {
    chrono::DateTime::parse_from_rfc2822(s)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp()).ok())
        .map(|secs| UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap_or(UNIX_EPOCH)
}

#[async_trait::async_trait]
impl HubOps for HubApiClient {
    async fn list_tree(&self, prefix: &str) -> Result<Vec<TreeEntry>> {
        self.list_tree(prefix).await
    }

    async fn has_children(&self, prefix: &str) -> Result<bool> {
        self.has_children(prefix).await
    }
    async fn head_file(&self, path: &str) -> Result<Option<HeadFileInfo>> {
        self.head_file(path).await
    }
    async fn batch_operations(&self, ops: &[BatchOp]) -> Result<()> {
        self.batch_operations(ops).await
    }
    async fn download_file_http(&self, path: &str, dest: &Path) -> Result<()> {
        self.download_file_http(path, dest).await
    }
    fn default_mtime(&self) -> SystemTime {
        self.default_mtime()
    }
    fn source(&self) -> &SourceKind {
        self.source()
    }
    fn is_repo(&self) -> bool {
        self.is_repo()
    }
    async fn probe_revision(&self) -> Result<String> {
        self.probe_revision().await
    }
    async fn probe_repo_commit(&self) -> Result<Option<String>> {
        let SourceKind::Repo {
            repo_type,
            repo_id,
            revision,
        } = &self.source
        else {
            return Ok(None);
        };
        let history = self.commit_history.lock().expect("commit history poisoned").next_poll();
        if history == CommitHistory::Available
            && let Some(head) = self.history_head(*repo_type, repo_id, revision).await
        {
            return Ok(Some(head));
        }
        let probe = self.revision_metadata().await?;
        if let Some(sha) = probe.sha.filter(|sha| !sha.is_empty()) {
            return Ok(Some(sha));
        }
        if history == CommitHistory::Unknown && probe.last_modified.is_some() {
            return Ok(self.history_head(*repo_type, repo_id, revision).await);
        }
        Ok(None)
    }
    async fn list_tree_at_commit(&self, prefix: &str, commit: &str) -> Result<Vec<TreeEntry>> {
        self.list_tree_at_commit(prefix, commit).await
    }
    async fn follow_events(
        &self,
        cursor: Option<&str>,
        since: Option<&str>,
    ) -> Result<Option<Box<dyn crate::follow::FollowStreamOps>>> {
        self.follow_events(cursor, since).await
    }
}

/// Repos expose the mounted revision's commit SHA, with its modification time
/// as a fallback for older Hub responses. Buckets expose their update time.
#[derive(serde::Deserialize)]
struct RevisionProbe {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default, rename = "lastModified")]
    last_modified: Option<String>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
}

// ── Token refresh ─────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum TokenKind {
    Read,
    Write,
}

pub struct HubTokenRefresher {
    hub_client: Arc<HubApiClient>,
    kind: TokenKind,
}

impl HubTokenRefresher {
    pub async fn fetch_initial(&self) -> Result<CasTokenInfo> {
        match self.kind {
            TokenKind::Read => self.hub_client.get_cas_token().await,
            TokenKind::Write => self.hub_client.get_cas_write_token().await,
        }
    }
}

#[async_trait::async_trait]
impl TokenRefresher for HubTokenRefresher {
    async fn refresh(&self) -> std::result::Result<TokenInfo, AuthError> {
        let jwt = self
            .fetch_initial()
            .await
            .map_err(|e| AuthError::TokenRefreshFailure(e.to_string()))?;
        Ok((jwt.access_token, jwt.exp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_op_add_file_serialization() {
        let op = BatchOp::AddFile {
            path: "data/file.bin".to_string(),
            xet_hash: "abc123def456".to_string(),
            mtime: 1700000000000,
            content_type: Some("application/octet-stream".to_string()),
        };

        let json = serde_json::to_string(&op).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Verify tag is camelCase "addFile"
        assert_eq!(parsed["type"], "addFile");
        assert_eq!(parsed["path"], "data/file.bin");
        assert_eq!(parsed["xetHash"], "abc123def456");
        assert_eq!(parsed["mtime"], 1700000000000u64);
        assert_eq!(parsed["contentType"], "application/octet-stream");
    }

    #[test]
    fn test_batch_op_delete_file_serialization() {
        let op = BatchOp::DeleteFile {
            path: "old/file.txt".to_string(),
        };

        let json = serde_json::to_string(&op).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["type"], "deleteFile");
        assert_eq!(parsed["path"], "old/file.txt");

        // Should not contain addFile-specific fields
        assert!(parsed.get("xetHash").is_none());
        assert!(parsed.get("mtime").is_none());
    }

    #[test]
    fn test_batch_op_add_file_no_content_type() {
        let op = BatchOp::AddFile {
            path: "readme.txt".to_string(),
            xet_hash: "hash999".to_string(),
            mtime: 1234567890000,
            content_type: None,
        };

        let json = serde_json::to_string(&op).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // contentType should be absent (skip_serializing_if = "Option::is_none")
        assert!(
            parsed.get("contentType").is_none(),
            "contentType should be omitted when None"
        );

        // Other fields should still be present
        assert_eq!(parsed["type"], "addFile");
        assert_eq!(parsed["path"], "readme.txt");
        assert_eq!(parsed["xetHash"], "hash999");
        assert_eq!(parsed["mtime"], 1234567890000u64);
    }

    #[test]
    fn test_mtime_parsers_fallback_to_unix_epoch_on_invalid_input() {
        assert_eq!(mtime_from_str("not-a-date"), UNIX_EPOCH);
        assert_eq!(mtime_from_http_date("still-not-a-date"), UNIX_EPOCH);
    }

    #[test]
    fn test_mtime_parsers_support_valid_formats() {
        let rfc3339 = mtime_from_str("2026-02-28T14:52:39Z");
        let http_date = mtime_from_http_date("Sat, 28 Feb 2026 14:52:39 GMT");
        assert!(rfc3339 > UNIX_EPOCH);
        assert!(http_date > UNIX_EPOCH);
        assert_eq!(rfc3339, http_date);
    }

    #[test]
    fn test_repo_type_from_str() {
        assert_eq!("model".parse::<RepoType>().unwrap(), RepoType::Model);
        assert_eq!("dataset".parse::<RepoType>().unwrap(), RepoType::Dataset);
        assert_eq!("space".parse::<RepoType>().unwrap(), RepoType::Space);
        assert!("unknown".parse::<RepoType>().is_err());
    }

    #[test]
    fn test_repo_type_api_prefix() {
        assert_eq!(RepoType::Model.api_prefix(), "models");
        assert_eq!(RepoType::Dataset.api_prefix(), "datasets");
        assert_eq!(RepoType::Space.api_prefix(), "spaces");
    }

    #[test]
    fn test_repo_type_resolve_prefix() {
        assert_eq!(RepoType::Model.resolve_prefix(), "");
        assert_eq!(RepoType::Dataset.resolve_prefix(), "datasets/");
        assert_eq!(RepoType::Space.resolve_prefix(), "spaces/");
    }

    // ── split_path_prefix tests ───────────────────────────────────────

    #[test]
    fn test_split_path_prefix_bucket_no_subfolder() {
        let (id, prefix) = split_path_prefix("user/bucket").unwrap();
        assert_eq!(id, "user/bucket");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_split_path_prefix_bucket_with_subfolder() {
        let (id, prefix) = split_path_prefix("user/bucket/a/b").unwrap();
        assert_eq!(id, "user/bucket");
        assert_eq!(prefix, "a/b");
    }

    #[test]
    fn test_split_path_prefix_bucket_single_subfolder() {
        let (id, prefix) = split_path_prefix("user/bucket/checkpoints").unwrap();
        assert_eq!(id, "user/bucket");
        assert_eq!(prefix, "checkpoints");
    }

    #[test]
    fn test_split_path_prefix_single_segment() {
        let (id, prefix) = split_path_prefix("gpt2").unwrap();
        assert_eq!(id, "gpt2");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_split_path_prefix_repo_with_subfolder() {
        let (id, prefix) = split_path_prefix("user/model/ckpt/v2").unwrap();
        assert_eq!(id, "user/model");
        assert_eq!(prefix, "ckpt/v2");
    }

    #[test]
    fn test_split_path_prefix_trailing_slash() {
        let (id, prefix) = split_path_prefix("user/bucket/checkpoints/").unwrap();
        assert_eq!(id, "user/bucket");
        assert_eq!(prefix, "checkpoints");
    }

    #[test]
    fn test_split_path_prefix_rejects_dotdot() {
        assert!(split_path_prefix("user/bucket/../other").is_err());
    }

    #[test]
    fn test_split_path_prefix_rejects_dot() {
        assert!(split_path_prefix("user/bucket/./foo").is_err());
    }

    // ── prefixed_path tests ───────────────────────

    fn make_test_client(prefix: &str, token_file: Option<PathBuf>) -> HubApiClient {
        let (client, head_client, follow_client) = make_clients("test");
        HubApiClient {
            commit_history: Mutex::default(),
            client,
            head_client,
            follow_client,
            endpoint: "https://huggingface.co".to_string(),
            token: Some("static-token".to_string()),
            token_file,
            token_file_cache: std::sync::Mutex::new(None),
            source: SourceKind::Bucket {
                bucket_id: "user/bucket".to_string(),
            },
            last_modified: UNIX_EPOCH,
            path_prefix: prefix.to_string(),
        }
    }

    #[test]
    fn test_encode_url_path_passthrough() {
        assert_eq!(encode_url_path("file.txt"), "file.txt");
        assert_eq!(encode_url_path("a/b/c.lance"), "a/b/c.lance");
        assert_eq!(encode_url_path(""), "");
    }

    #[test]
    fn test_encode_url_path_reserved_chars() {
        // object_store staging suffix: the `#` must not become a URL fragment
        assert_eq!(encode_url_path("_versions/1.manifest#1"), "_versions/1.manifest%231");
        assert_eq!(encode_url_path("a?b.txt"), "a%3Fb.txt");
        assert_eq!(encode_url_path("100%.txt"), "100%25.txt");
        assert_eq!(encode_url_path("a b/c d.txt"), "a%20b/c%20d.txt");
    }

    #[test]
    fn test_resolve_url_prefixes_and_encodes() {
        let c = make_test_client("", None);
        assert_eq!(
            c.resolve_url("_versions/1.manifest#1"),
            "https://huggingface.co/buckets/user/bucket/resolve/_versions/1.manifest%231"
        );
        let c = make_test_client("sub/dir", None);
        assert_eq!(
            c.resolve_url("a b.txt"),
            "https://huggingface.co/buckets/user/bucket/resolve/sub/dir/a%20b.txt"
        );
    }

    #[test]
    fn test_encode_url_revision_single_segment() {
        // Plain revisions pass through.
        assert_eq!(encode_url_revision("main"), "main");
        // Refs must stay a single path segment: the Hub 404s on the
        // unencoded form (extra segments parse as part of the file path).
        assert_eq!(encode_url_revision("refs/pr/1"), "refs%2Fpr%2F1");
        assert_eq!(encode_url_revision("refs/convert/parquet"), "refs%2Fconvert%2Fparquet");
    }

    #[test]
    fn test_resolve_url_encodes_revision() {
        let mut c = make_test_client("", None);
        c.source = SourceKind::Repo {
            repo_id: "user/model".to_string(),
            repo_type: RepoType::Model,
            revision: "refs/pr/1".to_string(),
        };
        assert_eq!(
            c.resolve_url("config.json"),
            "https://huggingface.co/user/model/resolve/refs%2Fpr%2F1/config.json"
        );
    }

    #[test]
    fn test_strict_descendant_rel() {
        // Real children (any depth) are in, relative path returned.
        assert_eq!(strict_descendant_rel("a/b.txt", "a"), Some("b.txt"));
        assert_eq!(strict_descendant_rel("a/b/c.txt", "a"), Some("b/c.txt"));
        // Raw S3 key-prefix matches are NOT children: the sibling staging
        // files of object_store writers (`1.manifest#1`) must not make
        // `1.manifest` look like a directory.
        assert_eq!(strict_descendant_rel("a/1.manifest#1", "a/1.manifest"), None);
        assert_eq!(strict_descendant_rel("ab.txt", "a"), None);
        // The path itself is not its own descendant.
        assert_eq!(strict_descendant_rel("a", "a"), None);
    }

    #[test]
    fn test_prefixed_path_empty_prefix() {
        let c = make_test_client("", None);
        assert_eq!(c.prefixed_path("file.txt"), "file.txt");
        assert_eq!(c.prefixed_path("a/b"), "a/b");
        assert_eq!(c.prefixed_path(""), "");
    }

    #[test]
    fn test_prefixed_path_with_prefix() {
        let c = make_test_client("sub/dir", None);
        assert_eq!(c.prefixed_path("file.txt"), "sub/dir/file.txt");
        assert_eq!(c.prefixed_path("a/b"), "sub/dir/a/b");
        assert_eq!(c.prefixed_path(""), "sub/dir");
    }

    // ── token file cache tests ────────────────────────────────────────

    fn cached_token(client: &HubApiClient) -> Option<String> {
        client.token_file_cache.lock().unwrap().as_ref().map(|(_, t)| t.clone())
    }

    fn test_token_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hf-mount-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn token_file_populates_cache_on_first_auth() {
        let dir = test_token_dir();
        let path = dir.join("token-pop");
        std::fs::write(&path, "file-token\n").unwrap();

        let client = make_test_client("", Some(path.clone()));
        let req = client.client.get("http://example.com");
        let _ = client.auth(req);

        assert_eq!(cached_token(&client).as_deref(), Some("file-token"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn token_file_cache_reused_within_ttl() {
        let dir = test_token_dir();
        let path = dir.join("token-ttl");
        std::fs::write(&path, "token-v1").unwrap();

        let client = make_test_client("", Some(path.clone()));
        let req = client.client.get("http://example.com");
        let _ = client.auth(req);
        assert_eq!(cached_token(&client).as_deref(), Some("token-v1"));

        // Update file, but cache should still serve old value.
        std::fs::write(&path, "token-v2").unwrap();
        let req = client.client.get("http://example.com");
        let _ = client.auth(req);
        assert_eq!(cached_token(&client).as_deref(), Some("token-v1"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn token_file_refreshes_after_ttl() {
        let dir = test_token_dir();
        let path = dir.join("token-refresh");
        std::fs::write(&path, "token-v1").unwrap();

        let client = make_test_client("", Some(path.clone()));
        let req = client.client.get("http://example.com");
        let _ = client.auth(req);

        // Force cache expiry by backdating the timestamp.
        {
            let mut cache = client.token_file_cache.lock().unwrap();
            if let Some((ref mut at, _)) = *cache {
                *at -= TOKEN_FILE_REFRESH + std::time::Duration::from_secs(1);
            }
        }

        std::fs::write(&path, "token-v2").unwrap();
        let req = client.client.get("http://example.com");
        let _ = client.auth(req);
        assert_eq!(cached_token(&client).as_deref(), Some("token-v2"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn token_file_missing_falls_back_to_static() {
        let path = PathBuf::from("/tmp/nonexistent-token-file-test");
        let client = make_test_client("", Some(path));
        let req = client.client.get("http://example.com");
        let _ = client.auth(req);
        assert!(cached_token(&client).is_none());
    }

    // ── retry / error helpers ─────────────────────────────────────────

    #[test]
    fn parse_retry_delay_ignores_zero_hint() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ratelimit", r#""hub_api";r=0;t=0"#.parse().unwrap());
        assert_eq!(parse_retry_delay(&headers), None);
    }

    #[test]
    fn parse_retry_delay_falls_back_to_retry_after() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "7".parse().unwrap());
        assert_eq!(parse_retry_delay(&headers), Some(std::time::Duration::from_secs(7)));
        // RateLimit wins when both are present.
        headers.insert("ratelimit", r#""hub_api";r=0;t=3"#.parse().unwrap());
        assert_eq!(parse_retry_delay(&headers), Some(std::time::Duration::from_secs(3)));
    }

    #[test]
    fn retry_delay_exponential_backoff() {
        assert_eq!(retry_delay(1), std::time::Duration::from_millis(500));
        assert_eq!(retry_delay(2), std::time::Duration::from_millis(1000));
        assert_eq!(retry_delay(3), std::time::Duration::from_millis(2000));
        // Saturates instead of overflowing on long retry loops.
        assert!(retry_delay(200) >= MAX_RETRY_DELAY);
    }

    #[test]
    fn is_retryable_status_covers_expected_codes() {
        use crate::error::is_retryable_status;
        assert!(is_retryable_status(408));
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(301));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn parse_retry_delay_extracts_t_value() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ratelimit", r#""hub_api";r=0;t=45"#.parse().unwrap());
        let duration = parse_retry_delay(&headers).unwrap();
        // Raw hint: each retry loop applies its own ceiling.
        assert_eq!(duration, std::time::Duration::from_secs(45));
    }

    #[test]
    fn parse_retry_delay_small_value() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ratelimit", r#""hub_api";r=0;t=5"#.parse().unwrap());
        let duration = parse_retry_delay(&headers).unwrap();
        assert_eq!(duration, std::time::Duration::from_secs(5));
    }

    #[test]
    fn parse_retry_delay_missing_header() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(parse_retry_delay(&headers).is_none());
    }

    #[test]
    fn parse_retry_delay_no_t_param() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ratelimit", r#""hub_api";r=50"#.parse().unwrap());
        assert!(parse_retry_delay(&headers).is_none());
    }

    #[test]
    fn error_hub_constructors() {
        let err = Error::hub("test error");
        assert!(matches!(err, Error::Hub { status: None, .. }));
        assert!(err.to_string().contains("test error"));

        let err = Error::hub_status(404, "not found");
        assert!(matches!(err, Error::Hub { status: Some(404), .. }));
        assert!(err.to_string().contains("404"));
        assert!(err.to_string().contains("not found"));
    }

    // ── send_with_retry integration tests ─────────────────────────────

    /// Minimal HTTP server that responds with a given sequence of status codes.
    async fn mock_server(responses: Vec<u16>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");

        tokio::spawn(async move {
            for status in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let reason = match status {
                    200 => "OK",
                    304 => "Not Modified",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Unknown",
                };
                let body = format!("status {status}");
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.ok();
                stream.shutdown().await.ok();
            }
        });

        url
    }

    #[tokio::test]
    async fn send_with_retry_success_on_first_try() {
        let url = mock_server(vec![200]).await;
        let client = Client::new();
        let resp = send_with_retry(|| client.get(&url), "test", false).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn send_with_retry_retries_on_503_then_succeeds() {
        let url = mock_server(vec![503, 200]).await;
        let client = Client::new();
        let resp = send_with_retry(|| client.get(&url), "test", false).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn send_with_retry_retries_on_429_then_succeeds() {
        let url = mock_server(vec![429, 200]).await;
        let client = Client::new();
        let resp = send_with_retry(|| client.get(&url), "test", false).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn send_with_retry_gives_up_after_max_retries() {
        let url = mock_server(vec![503, 503, 503]).await;
        let client = Client::new();
        let result = send_with_retry(|| client.get(&url), "test", false).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Error::Hub { status: Some(503), .. }));
    }

    #[tokio::test]
    async fn send_with_retry_no_retry_on_404() {
        let url = mock_server(vec![404]).await;
        let client = Client::new();
        let result = send_with_retry(|| client.get(&url), "test", false).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Hub { status: Some(404), .. }));
    }

    #[tokio::test]
    async fn send_with_retry_304_returned_as_error() {
        let url = mock_server(vec![304]).await;
        let client = Client::new();
        let result = send_with_retry(|| client.get(&url), "test", false).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Hub { status: Some(304), .. }));
    }

    #[tokio::test]
    async fn send_with_retry_accepts_redirects_when_enabled() {
        let url = mock_server(vec![302]).await;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = send_with_retry(|| client.get(&url), "test", true).await.unwrap();
        assert_eq!(resp.status(), 302);
    }

    #[tokio::test]
    async fn send_with_retry_rejects_redirects_when_disabled() {
        let url = mock_server(vec![302]).await;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let result = send_with_retry(|| client.get(&url), "test", false).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Hub { status: Some(302), .. }));
    }

    // ── head_file tests ───────────────────────────────────────────────

    /// HEAD `file.txt` against a server that answers once with `status` and `headers`.
    async fn head_file_with_response(status: &str, headers: &str) -> HeadFileInfo {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let response = format!("HTTP/1.1 {status}\r\n{headers}connection: close\r\n\r\n");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            stream.write_all(response.as_bytes()).await.ok();
            stream.shutdown().await.ok();
        });

        let mut client = make_test_client("", None);
        client.endpoint = format!("http://{addr}");
        client.head_file("file.txt").await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn head_file_redirect_ignores_content_length_and_etag() {
        // Plain file in a public repo: the 307 to /api/resolve-cache carries
        // the redirect body's content-length (246), not the file's (665).
        let info = head_file_with_response(
            "307 Temporary Redirect",
            "content-length: 246\r\netag: \"redirect\"\r\nlocation: /api/resolve-cache/file.txt\r\n",
        )
        .await;
        assert_eq!(info.size, None);
        assert_eq!(info.etag, None);
    }

    #[tokio::test]
    async fn head_file_redirect_uses_linked_headers() {
        let info = head_file_with_response(
            "307 Temporary Redirect",
            "content-length: 246\r\nx-linked-size: 665\r\nx-linked-etag: \"10c6\"\r\nlocation: /api/resolve-cache/file.txt\r\n",
        )
        .await;
        assert_eq!(info.size, Some(665));
        assert_eq!(info.etag.as_deref(), Some("10c6"));
    }

    #[tokio::test]
    async fn head_file_direct_response_uses_content_length() {
        // Private or gated repo: the plain file is served directly with a 200.
        let info = head_file_with_response("200 OK", "content-length: 665\r\netag: \"10c6\"\r\n").await;
        assert_eq!(info.size, Some(665));
        assert_eq!(info.etag.as_deref(), Some("10c6"));
    }

    // ── probe_repo tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn probe_repo_identifies_model() {
        // probe_repo hits /api/models first → 200 means it's a model.
        let url = mock_server(vec![200]).await;
        let client = Client::new();
        let result = probe_repo(&client, &url, "user/thing", None, &None).await;
        assert_eq!(result, Some(RepoType::Model));
    }

    #[tokio::test]
    async fn probe_repo_falls_through_to_dataset() {
        // models 404 → datasets 200 → identified as dataset.
        let url = mock_server(vec![404, 200]).await;
        let client = Client::new();
        let result = probe_repo(&client, &url, "user/thing", None, &None).await;
        assert_eq!(result, Some(RepoType::Dataset));
    }

    #[tokio::test]
    async fn probe_repo_returns_none_when_no_endpoint_matches() {
        // All three repo endpoints return 404 → not a repo.
        let url = mock_server(vec![404, 404, 404]).await;
        let client = Client::new();
        let result = probe_repo(&client, &url, "user/thing", None, &None).await;
        assert_eq!(result, None);
    }
}
