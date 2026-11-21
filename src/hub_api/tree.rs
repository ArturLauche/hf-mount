//! Paginated, mount-relative directory listings for Hub repos and buckets.

use std::collections::HashSet;

use reqwest::{Response, Url, header::LINK};
use serde::{Deserialize, de::DeserializeOwned};

use super::{HubApiClient, SourceKind, encode_url_path, encode_url_revision, send_with_retry, strict_descendant_rel};
use crate::error::{Error, Result};

/// Unified tree entry exposed to the rest of the codebase.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub size: Option<u64>,
    pub xet_hash: Option<String>,
    /// Git blob OID (same value as ETag on resolve endpoint).
    #[serde(default)]
    pub oid: Option<String>,
    pub mtime: Option<String>,
}

/// Raw tree entry from the repo `/tree` API (different shape from bucket tree).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoTreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    size: Option<u64>,
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    xet_hash: Option<String>,
    #[serde(default)]
    lfs: Option<LfsInfo>,
    #[serde(default)]
    last_commit: Option<CommitInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitInfo {
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LfsInfo {
    size: u64,
}

impl From<RepoTreeEntry> for TreeEntry {
    fn from(entry: RepoTreeEntry) -> Self {
        Self {
            path: entry.path,
            entry_type: entry.entry_type,
            // For LFS files the top-level size can describe the pointer.
            size: entry.lfs.map(|lfs| lfs.size).or(entry.size),
            xet_hash: entry.xet_hash,
            oid: entry.oid,
            mtime: entry.last_commit.and_then(|commit| commit.date),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeRead {
    All,
    FirstChild,
}

impl HubApiClient {
    /// List the immediate children of a mount-relative directory. Filter raw
    /// S3 prefix matches (e.g. `a/b#1` is not under `a/b/`) before returning.
    pub async fn list_tree(&self, prefix: &str) -> Result<Vec<TreeEntry>> {
        self.read_tree(prefix, TreeRead::All).await
    }

    pub(super) async fn has_children(&self, prefix: &str) -> Result<bool> {
        Ok(!self.read_tree(prefix, TreeRead::FirstChild).await?.is_empty())
    }

    async fn read_tree(&self, prefix: &str, mode: TreeRead) -> Result<Vec<TreeEntry>> {
        let api_prefix = self.prefixed_path(prefix);
        let subdir = if api_prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", encode_url_path(&api_prefix))
        };
        match &self.source {
            SourceKind::Bucket { bucket_id } => {
                let url = format!(
                    "{}/api/buckets/{bucket_id}/tree{subdir}?recursive=false&limit=5000",
                    self.endpoint
                );
                // Buckets can return many raw-prefix siblings. Keep the normal
                // page size so a negative probe does not need one request per sibling.
                self.tree_pages::<TreeEntry>(&url, &api_prefix, mode).await
            }
            SourceKind::Repo {
                repo_id,
                repo_type,
                revision,
            } => {
                let limit = if mode == TreeRead::FirstChild { 1 } else { 1000 };
                let url = format!(
                    "{}/api/{}/{repo_id}/tree/{}{subdir}?limit={limit}",
                    self.endpoint,
                    repo_type.api_prefix(),
                    encode_url_revision(revision)
                );
                self.tree_pages::<RepoTreeEntry>(&url, &api_prefix, mode).await
            }
        }
    }

    async fn tree_pages<T: DeserializeOwned + Into<TreeEntry>>(
        &self,
        first: &str,
        api_prefix: &str,
        mode: TreeRead,
    ) -> Result<Vec<TreeEntry>> {
        let mut url = Url::parse(first).map_err(|e| Error::hub(format!("invalid tree listing URL: {e}")))?;
        let hub_origin = url.origin();
        let mut visited = HashSet::new();
        let mut entries = Vec::new();
        loop {
            let resp = send_with_retry(
                || {
                    let req = self.client.get(url.clone());
                    if url.origin() == hub_origin {
                        self.auth(req)
                    } else {
                        req
                    }
                },
                "tree listing",
                false,
            )
            .await?;
            let next = next_page(&resp);
            let page: Vec<T> = resp.json().await?;
            for mut entry in page.into_iter().map(Into::<TreeEntry>::into) {
                if !api_prefix.is_empty() && strict_descendant_rel(&entry.path, api_prefix).is_none() {
                    continue;
                }
                if !self.path_prefix.is_empty() {
                    // Every accepted path starts with the mount prefix and its slash.
                    entry.path.drain(..self.path_prefix.len() + 1);
                }
                entries.push(entry);
                if mode == TreeRead::FirstChild {
                    return Ok(entries);
                }
            }
            let Some(next) = next? else { return Ok(entries) };
            visited.insert(url);
            if visited.contains(&next) {
                return Err(Error::hub("tree listing repeated a pagination link"));
            }
            url = next;
        }
    }
}

/// Link references are relative to the effective response URL (after redirects).
/// An invalid continuation must fail the listing: polling treats a successful
/// but truncated tree as evidence that the missing children have been deleted.
fn next_page(response: &Response) -> Result<Option<Url>> {
    for header in response.headers().get_all(LINK) {
        let header = header
            .to_str()
            .map_err(|_| Error::hub("invalid tree listing Link header"))?;
        let links =
            nom_rfc8288::complete::link_lenient(header).map_err(|_| Error::hub("invalid tree listing Link header"))?;
        for link in links.into_iter().flatten() {
            let is_next = link.params.iter().any(|p| {
                p.key.eq_ignore_ascii_case("rel")
                    && p.val
                        .as_deref()
                        .is_some_and(|v| v.split_ascii_whitespace().any(|r| r.eq_ignore_ascii_case("next")))
            });
            if is_next {
                let mut next = response
                    .url()
                    .join(link.url)
                    .map_err(|e| Error::hub(format!("invalid tree listing continuation: {e}")))?;
                if !matches!(next.scheme(), "http" | "https")
                    || !next.username().is_empty()
                    || next.password().is_some()
                {
                    return Err(Error::hub("invalid tree listing continuation URL"));
                }
                // Fragments never reach the server and cannot identify a new page.
                next.set_fragment(None);
                return Ok(Some(next));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
