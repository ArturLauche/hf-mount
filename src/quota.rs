//! Namespace storage capacity for WinFsp and FUSE. See docs/storage-quota.md.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::hub_api::{HubApiClient, SourceKind, pagination};

/// Placeholder for a scope with no verifiable capacity, including best-effort
/// public storage. This number is not a Hub limit.
pub const FALLBACK_BYTES: u64 = 42 * (1 << 50);
const TIMEOUT: Duration = Duration::from_secs(4);
const DEFAULT_REFRESH_SECS: u64 = 15 * 60;
const GB: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaScope {
    Public,
    Private,
}

impl From<bool> for QuotaScope {
    fn from(private: bool) -> Self {
        if private { Self::Private } else { Self::Public }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaSource {
    /// Hub usage summed over the scope, with the plan's included allowance.
    HubEstimate,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaSnapshot {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub remaining_bytes: u64,
    pub scope: QuotaScope,
    pub source: QuotaSource,
    pub fetched_at: SystemTime,
}

impl QuotaSnapshot {
    fn new(scope: QuotaScope, source: QuotaSource, total_bytes: u64, used_bytes: u64) -> Self {
        Self {
            total_bytes,
            used_bytes,
            // Clamp to the included allowance. Paid plans may still accept
            // pay-as-you-go writes above it, but the Hub does not expose that
            // effective limit, and free space must never exceed total capacity.
            remaining_bytes: total_bytes.saturating_sub(used_bytes),
            scope,
            source,
            fetched_at: SystemTime::now(),
        }
    }

    fn fallback(scope: QuotaScope) -> Self {
        Self::new(scope, QuotaSource::Fallback, FALLBACK_BYTES, 0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct QuotaCache(Arc<QuotaState>);

#[derive(Debug, Default)]
struct QuotaState {
    // None means the source's visibility has not been determined.
    snapshot: RwLock<Option<QuotaSnapshot>>,
    refresh: OnceLock<AbortHandle>,
}

impl Drop for QuotaState {
    fn drop(&mut self) {
        if let Some(task) = self.refresh.get() {
            task.abort();
        }
    }
}

impl QuotaCache {
    pub(crate) fn start(runtime: &Handle, hub: Arc<HubApiClient>) -> Self {
        let (_, _, private) = hub.quota_context();
        let mut scope = private.map(QuotaScope::from);
        let cache = Self(Arc::new(QuotaState {
            snapshot: RwLock::new(scope.map(QuotaSnapshot::fallback)),
            refresh: OnceLock::new(),
        }));
        let state = Arc::downgrade(&cache.0);
        let refresh_secs = std::env::var("HF_MOUNT_QUOTA_REFRESH_SECS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&secs| secs > 0)
            .map(u64::from)
            .unwrap_or(DEFAULT_REFRESH_SECS);
        let task = runtime.spawn(async move {
            let mut first = true;
            loop {
                let estimate = tokio::time::timeout(TIMEOUT, refresh(&hub, &mut scope, first))
                    .await
                    .ok()
                    .flatten();
                if let Some(state) = state.upgrade() {
                    let mut snapshot = state.snapshot.write().expect("quota cache poisoned");
                    publish(&mut snapshot, estimate, scope);
                } else {
                    break;
                }
                first = false;
                tokio::time::sleep(Duration::from_secs(refresh_secs)).await;
            }
        });
        let _ = cache.0.refresh.set(task.abort_handle());
        cache
    }

    /// Publish total and free space from one snapshot, without waiting for a
    /// refresh. A callback racing the short write critical section can use
    /// the placeholder until the next query.
    pub fn volume_bytes(&self) -> (u64, u64) {
        self.snapshot().map_or((FALLBACK_BYTES, FALLBACK_BYTES), |quota| {
            (quota.total_bytes, quota.remaining_bytes)
        })
    }

    pub fn snapshot(&self) -> Option<QuotaSnapshot> {
        *self.0.snapshot.try_read().ok()?
    }

    pub fn stop(&self) {
        if let Some(task) = self.0.refresh.get() {
            task.abort();
        }
    }
}

/// Keep the last good estimate across transient failures. Only the first
/// fetch and a verified visibility change may replace it with the placeholder.
fn publish(snapshot: &mut Option<QuotaSnapshot>, estimate: Option<QuotaSnapshot>, scope: Option<QuotaScope>) {
    if let Some(estimate) = estimate {
        *snapshot = Some(estimate);
    } else if snapshot
        .as_ref()
        .is_none_or(|current| scope.is_some_and(|scope| current.scope != scope))
    {
        *snapshot = scope.map(QuotaSnapshot::fallback);
    }
}

async fn refresh(hub: &Arc<HubApiClient>, scope: &mut Option<QuotaScope>, first: bool) -> Option<QuotaSnapshot> {
    let Some(token) = hub.quota_token().await.filter(|token| !token.is_empty()) else {
        *scope = Some(QuotaScope::Public);
        return None;
    };
    if !first || scope.is_none() {
        let body: serde_json::Value = success(hub.visibility_request(&token).send().await.ok()?)?
            .json()
            .await
            .ok()?;
        *scope = body["private"].as_bool().map(QuotaScope::from);
    }
    let id = match hub.source() {
        SourceKind::Bucket { bucket_id } => bucket_id,
        SourceKind::Repo { repo_id, .. } => repo_id,
    };
    let (namespace, _) = id.split_once('/')?;
    let (client, endpoint, _) = hub.quota_context();
    fetch(client, endpoint, &token, namespace, (*scope)?).await
}

/// Get a namespace's storage estimate when its usage and included allowance
/// can both be established. The time budget covers identity and all pages.
pub async fn fetch_quota(
    endpoint: &str,
    token: Option<&str>,
    namespace: &str,
    scope: QuotaScope,
) -> Option<QuotaSnapshot> {
    let token = token?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    tokio::time::timeout(TIMEOUT, fetch(&client, endpoint, token, namespace, scope))
        .await
        .ok()
        .flatten()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    #[serde(default)]
    is_pro: bool,
    // Keep raw values: one malformed organization must not disable quota for
    // the user's own namespace.
    #[serde(default)]
    orgs: Option<Vec<serde_json::Value>>,
    auth: Auth,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Organization {
    name: String,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    role_in_org: Option<String>,
    #[serde(default)]
    security_restrictions: serde_json::Value,
}

impl Organization {
    /// The schema marks `securityRestrictions` optional. Treat a missing or
    /// null value as unrestricted and any unexpected shape as restricted.
    fn is_unrestricted(&self) -> bool {
        match &self.security_restrictions {
            serde_json::Value::Null => true,
            serde_json::Value::Array(restrictions) => restrictions.is_empty(),
            _ => false,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Auth {
    #[serde(default)]
    access_token: Option<AccessToken>,
}

#[derive(Deserialize)]
struct AccessToken {
    role: String,
}

#[derive(Deserialize)]
struct RepoStorage {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    visibility: String,
    storage: u64,
}

fn success(response: Response) -> Option<Response> {
    response.status().is_success().then_some(response)
}

async fn fetch(
    client: &Client,
    endpoint: &str,
    token: &str,
    namespace: &str,
    scope: QuotaScope,
) -> Option<QuotaSnapshot> {
    // Best-effort, grants and add-ons cannot be inferred from an account plan.
    if scope == QuotaScope::Public || token.is_empty() || namespace.is_empty() {
        return None;
    }
    let identity: Identity = success(
        client
            .get(format!("{}/api/whoami-v2", endpoint.trim_end_matches('/')))
            .bearer_auth(token)
            .send()
            .await
            .ok()?,
    )?
    .json()
    .await
    .ok()?;
    // A resource-scoped token can produce a partial listing. Require a
    // namespace-wide credential and, for orgs, an unrestricted admin role.
    let role = identity.auth.access_token.as_ref().map(|token| token.role.as_str());
    if identity.kind != "user" || !role.is_some_and(|role| matches!(role, "read" | "write")) {
        return None;
    }
    let mut listing = Url::parse(endpoint).ok()?;
    let total = {
        let mut path = listing.path_segments_mut().ok()?;
        path.pop_if_empty().push("api");
        if identity.name.eq_ignore_ascii_case(namespace) {
            path.extend(["settings", "repositories"]);
            if identity.is_pro { 1000 * GB } else { 100 * GB }
        } else {
            let org = identity
                .orgs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|value| serde_json::from_value::<Organization>(value.clone()).ok())
                .find(|org| org.name.eq_ignore_ascii_case(namespace))?;
            if org.role_in_org.as_deref() != Some("admin") || !org.is_unrestricted() {
                return None;
            }
            // The Hub omits plan for free organizations. Paid plans depend
            // on the billed seat count, which whoami does not expose.
            if org.plan.is_some() {
                return None;
            }
            path.extend(["organizations", &org.name, "settings", "repositories"]);
            100 * GB
        }
    };

    let mut page = listing.clone();
    let mut seen_pages = HashSet::new();
    let mut seen_repos = HashSet::new();
    let mut used = 0u64;
    loop {
        if page.origin() != listing.origin() || page.path() != listing.path() || !seen_pages.insert(page.clone()) {
            return None;
        }
        let response = success(client.get(page).bearer_auth(token).send().await.ok()?)?;
        let next = pagination::next_page(&response).ok()?;
        let entries: Vec<serde_json::Value> = response.json().await.ok()?;
        for entry in entries {
            // Filter before parsing so a malformed entry in another namespace
            // cannot discard this namespace's estimate.
            let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some((owner, _)) = id.split_once('/') else {
                continue;
            };
            if !owner.eq_ignore_ascii_case(namespace) {
                continue;
            }
            let repo: RepoStorage = serde_json::from_value(entry).ok()?;
            match repo.visibility.as_str() {
                "private" => {
                    if !seen_repos.insert((repo.kind, repo.id)) {
                        return None;
                    }
                    used = used.checked_add(repo.storage)?;
                }
                "public" => {}
                // The Hub doesn't document how protected Space storage is
                // billed. A nonempty entry could affect this scope's usage.
                _ if repo.storage == 0 => {}
                _ => return None,
            }
        }
        match next {
            Some(next) => page = next,
            None => break,
        }
    }
    Some(QuotaSnapshot::new(scope, QuotaSource::HubEstimate, total, used))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_mocks::serve_hub_requests;

    const IDENTITY: &str = r#"{
        "type":"user","name":"alice","isPro":true,
        "orgs":[
            {"name":"lab","roleInOrg":"admin"},
            {"name":"paid-lab","roleInOrg":"admin","plan":"team"},
            {"name":"other-lab","roleInOrg":"read"}
        ],
        "auth":{"accessToken":{"role":"read"}}
    }"#;

    struct Reply {
        path: &'static str,
        status: u16,
        body: &'static str,
        headers: &'static str,
    }

    fn ok(path: &'static str, body: &'static str) -> Reply {
        Reply {
            path,
            status: 200,
            body,
            headers: "",
        }
    }

    // As in hub_api tests, serve scripted responses over loopback. Assert
    // paths so that mixing up personal, org and pagination calls fails.
    async fn mock_server(replies: Vec<Reply>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let mut replies = replies.into_iter();
        serve_hub_requests(replies.len(), move |path| {
            let reply = replies.next().unwrap();
            assert_eq!(path, reply.path);
            (
                reqwest::StatusCode::from_u16(reply.status).unwrap(),
                reply.headers.into(),
                reply.body.into(),
            )
        })
        .await
    }

    async fn wait_for_snapshot(cache: &QuotaCache, predicate: impl Fn(QuotaSnapshot) -> bool) -> QuotaSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(snapshot) = cache.snapshot().filter(|snapshot| predicate(*snapshot)) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn private_repo_uses_its_resolved_namespace_and_all_storage_pages() {
        use crate::hub_api::RepoType;
        let (endpoint, server) = mock_server(vec![
            ok(
                "/api/models/alias/revision/main?expand=sha&expand=lastModified",
                r#"{"id":"alice/model"}"#,
            ),
            ok(
                "/api/models/alice/model/revision/main?expand=private",
                r#"{"id":"alice/model","private":true}"#,
            ),
            ok("/api/whoami-v2", IDENTITY),
            Reply {
                headers: "Link: <?cursor=second>; rel=\"next\"\r\n",
                ..ok(
                    "/api/settings/repositories",
                    r#"[
                    {"id":"alice/model","type":"model","visibility":"private","storage":500},
                    {"id":"alice/public","type":"dataset","visibility":"public","storage":9000000000000}
                ]"#,
                )
            },
            ok(
                "/api/settings/repositories?cursor=second",
                r#"[
                {"id":"alice/files","type":"bucket","visibility":"private","storage":2000},
                {"id":"lab/files","type":"bucket","visibility":"private","storage":999}
            ]"#,
            ),
        ])
        .await;
        let hub = HubApiClient::from_source(
            &endpoint,
            Some("mock-token"),
            None,
            SourceKind::Repo {
                repo_id: "alias".into(),
                repo_type: RepoType::Model,
                revision: "main".into(),
            },
            "mount-prefix".into(),
            "test",
        )
        .await
        .unwrap();
        let cache = QuotaCache::start(&Handle::current(), hub);
        let estimate = wait_for_snapshot(&cache, |quota| quota.source == QuotaSource::HubEstimate).await;
        assert_eq!(estimate.scope, QuotaScope::Private);
        assert_eq!(estimate.used_bytes, 2500);
        assert_eq!(cache.volume_bytes(), (1000 * GB, 1000 * GB - 2500));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn private_org_bucket_uses_org_quota_and_clamps_overage() {
        let (endpoint, server) = mock_server(vec![
            ok("/api/buckets/lab/files", r#"{"id":"lab/files","private":true}"#),
            ok("/api/whoami-v2", IDENTITY),
            ok(
                "/api/organizations/lab/settings/repositories",
                r#"[{"id":"lab/files","type":"bucket","visibility":"private","storage":110000000000}]"#,
            ),
        ])
        .await;
        let hub = HubApiClient::from_source(
            &endpoint,
            Some("mock-token"),
            None,
            SourceKind::Bucket {
                bucket_id: "lab/files".into(),
            },
            String::new(),
            "test",
        )
        .await
        .unwrap();
        let cache = QuotaCache::start(&Handle::current(), hub);
        let snapshot = wait_for_snapshot(&cache, |quota| quota.source == QuotaSource::HubEstimate).await;
        assert_eq!(snapshot.used_bytes, 110 * GB);
        assert_eq!(cache.volume_bytes(), (100 * GB, 0));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unknown_paid_or_inaccessible_namespaces_do_not_use_personal_quota() {
        for namespace in ["paid-lab", "other-lab", "stranger"] {
            let (endpoint, server) = mock_server(vec![ok("/api/whoami-v2", IDENTITY)]).await;
            assert!(
                fetch_quota(&endpoint, Some("mock-token"), namespace, QuotaScope::Private)
                    .await
                    .is_none()
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn public_and_anonymous_mounts_use_public_fallback() {
        for token in [Some("mock-token"), None] {
            let (endpoint, server) = mock_server(vec![ok("/api/buckets/alice/files", r#"{"private":false}"#)]).await;
            let hub = HubApiClient::from_source(
                &endpoint,
                token,
                None,
                SourceKind::Bucket {
                    bucket_id: "alice/files".into(),
                },
                String::new(),
                "test",
            )
            .await
            .unwrap();
            let cache = QuotaCache::start(&Handle::current(), hub);
            let snapshot = wait_for_snapshot(&cache, |quota| quota.scope == QuotaScope::Public).await;
            assert_eq!(snapshot.source, QuotaSource::Fallback);
            assert_eq!(cache.volume_bytes(), (FALLBACK_BYTES, FALLBACK_BYTES));
            server.await.unwrap();
        }

        // Without a credential, even older Hub implementations that omit
        // privacy metadata get a Public fallback.
        let hub = HubApiClient::new("http://127.0.0.1:1", None, "alice/files", "test");
        let cache = QuotaCache::start(&Handle::current(), hub);
        let snapshot = wait_for_snapshot(&cache, |_| true).await;
        assert_eq!(
            (snapshot.scope, snapshot.source),
            (QuotaScope::Public, QuotaSource::Fallback)
        );
    }

    #[test]
    fn failed_refresh_keeps_the_last_good_estimate() {
        let estimate = QuotaSnapshot::new(QuotaScope::Private, QuotaSource::HubEstimate, 100 * GB, 43 * GB);
        let mut snapshot = Some(estimate);

        publish(&mut snapshot, None, Some(QuotaScope::Private));
        assert_eq!(snapshot, Some(estimate));

        // A verified visibility change still replaces the estimate.
        publish(&mut snapshot, None, Some(QuotaScope::Public));
        let fallback = snapshot.unwrap();
        assert_eq!(fallback.scope, QuotaScope::Public);
        assert_eq!(fallback.source, QuotaSource::Fallback);
        assert_eq!(fallback.total_bytes, FALLBACK_BYTES);
    }

    #[test]
    fn failed_first_refresh_publishes_the_scope_fallback() {
        let mut snapshot = None;
        publish(&mut snapshot, None, Some(QuotaScope::Private));
        let fallback = snapshot.unwrap();
        assert_eq!(fallback.scope, QuotaScope::Private);
        assert_eq!(fallback.source, QuotaSource::Fallback);
        assert_eq!(fallback.total_bytes, FALLBACK_BYTES);
    }

    #[tokio::test]
    async fn org_entries_with_null_restrictions_do_not_disable_the_estimate() {
        const TOLERANT_IDENTITY: &str = r#"{
            "type":"user","name":"alice","isPro":false,
            "orgs":[
                {"name":"broken","plan":123},
                {"name":"lab","roleInOrg":"admin","securityRestrictions":null}
            ],
            "auth":{"accessToken":{"role":"read"}}
        }"#;
        let (endpoint, server) = mock_server(vec![
            ok("/api/whoami-v2", TOLERANT_IDENTITY),
            ok(
                "/api/organizations/lab/settings/repositories",
                r#"[{"id":"lab/model","type":"model","visibility":"private","storage":42}]"#,
            ),
        ])
        .await;
        let snapshot = fetch_quota(&endpoint, Some("mock-token"), "lab", QuotaScope::Private)
            .await
            .unwrap();
        assert_eq!(snapshot.used_bytes, 42);
        assert_eq!(snapshot.total_bytes, 100 * GB);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn null_org_list_does_not_disable_a_personal_estimate() {
        const NULL_ORGS_IDENTITY: &str = r#"{
            "type":"user","name":"alice","isPro":false,"orgs":null,
            "auth":{"accessToken":{"role":"read"}}
        }"#;
        let (endpoint, server) = mock_server(vec![
            ok("/api/whoami-v2", NULL_ORGS_IDENTITY),
            ok(
                "/api/settings/repositories",
                r#"[{"id":"alice/model","type":"model","visibility":"private","storage":7}]"#,
            ),
        ])
        .await;
        let snapshot = fetch_quota(&endpoint, Some("mock-token"), "alice", QuotaScope::Private)
            .await
            .unwrap();
        assert_eq!(snapshot.used_bytes, 7);
        assert_eq!(snapshot.total_bytes, 100 * GB);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_entries_in_other_namespaces_are_ignored() {
        let (endpoint, server) = mock_server(vec![
            ok("/api/whoami-v2", IDENTITY),
            ok(
                "/api/settings/repositories",
                r#"[
                    {"id":"bob/broken","type":null,"visibility":null,"storage":null},
                    {"id":"alice/model","type":"model","visibility":"private","storage":500}
                ]"#,
            ),
        ])
        .await;
        let snapshot = fetch_quota(&endpoint, Some("mock-token"), "alice", QuotaScope::Private)
            .await
            .unwrap();
        assert_eq!(snapshot.used_bytes, 500);
        assert_eq!(snapshot.total_bytes, 1000 * GB);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_entry_in_the_mounted_namespace_falls_back() {
        let (endpoint, server) = mock_server(vec![
            ok("/api/whoami-v2", IDENTITY),
            ok(
                "/api/settings/repositories",
                r#"[{"id":"alice/model","type":"model","visibility":"private","storage":null}]"#,
            ),
        ])
        .await;
        assert!(
            fetch_quota(&endpoint, Some("mock-token"), "alice", QuotaScope::Private)
                .await
                .is_none()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unauthorized_usage_falls_back() {
        let (endpoint, server) = mock_server(vec![
            ok("/api/buckets/alice/files", r#"{"private":true}"#),
            ok("/api/whoami-v2", IDENTITY),
            Reply {
                status: 401,
                ..ok("/api/settings/repositories", "{}")
            },
        ])
        .await;
        // Start without cached visibility so the first refresh has to resolve
        // the scope before the listing fails.
        let hub = HubApiClient::new(&endpoint, Some("mock-token"), "alice/files", "test");
        let cache = QuotaCache::start(&Handle::current(), hub);
        let snapshot = wait_for_snapshot(&cache, |quota| {
            quota.scope == QuotaScope::Private && quota.source == QuotaSource::Fallback
        })
        .await;
        assert_eq!(snapshot.used_bytes, 0);
        assert_eq!(cache.volume_bytes(), (FALLBACK_BYTES, FALLBACK_BYTES));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn quota_request_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let accepted = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            // Keep the connection open while the client awaits a response.
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(
            TIMEOUT + Duration::from_secs(1),
            fetch_quota(&endpoint, Some("mock-token"), "alice", QuotaScope::Private),
        )
        .await
        .unwrap();
        assert!(result.is_none());
        accepted.abort();
    }
}
