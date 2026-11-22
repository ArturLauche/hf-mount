//! Paginated, mount-relative directory listings for Hub repos and buckets.

use std::{borrow::Cow, collections::HashSet};

use percent_encoding::percent_decode_str;
use reqwest::Url;
use serde::{Deserialize, de::DeserializeOwned};

use super::{HubApiClient, SourceKind, encode_url_path, encode_url_revision, send_with_retry, strict_descendant_rel};
use crate::error::{Error, Result};

mod pagination;

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
        self.read_tree(prefix, TreeRead::All, None).await
    }

    pub(super) async fn has_children(&self, prefix: &str) -> Result<bool> {
        Ok(!self.read_tree(prefix, TreeRead::FirstChild, None).await?.is_empty())
    }

    /// List a repo directory at a previously resolved commit.
    pub async fn list_tree_at_commit(&self, prefix: &str, commit: &str) -> Result<Vec<TreeEntry>> {
        if !self.is_repo() {
            return Err(Error::hub("commit-pinned tree listing requires a repo"));
        }
        self.read_tree(prefix, TreeRead::All, Some(commit)).await
    }

    async fn read_tree(&self, prefix: &str, mode: TreeRead, commit: Option<&str>) -> Result<Vec<TreeEntry>> {
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
                self.tree_pages::<TreeEntry>(&url, &api_prefix, mode, None).await
            }
            SourceKind::Repo {
                repo_id,
                repo_type,
                revision,
            } => {
                let limit = if mode == TreeRead::FirstChild { 1 } else { 1000 };
                let tree_root = format!("{}/api/{}/{repo_id}/tree/", self.endpoint, repo_type.api_prefix());
                let revision = encode_url_revision(commit.unwrap_or(revision));
                let url = format!("{tree_root}{revision}{subdir}?limit={limit}");
                let pinned_root = commit.map(|_| parse_tree_url(&tree_root)).transpose()?;
                self.tree_pages::<RepoTreeEntry>(&url, &api_prefix, mode, pinned_root.as_ref())
                    .await
            }
        }
    }

    async fn tree_pages<T: DeserializeOwned + Into<TreeEntry>>(
        &self,
        first: &str,
        api_prefix: &str,
        mode: TreeRead,
        pinned_root: Option<&Url>,
    ) -> Result<Vec<TreeEntry>> {
        let mut url = parse_tree_url(first)?;
        let hub_origin = url.origin();
        let pinned_dir = pinned_root.map(|root| (root, url.path().to_owned()));
        let check_pin = |page: &Url| {
            // Opaque continuation URLs can be served elsewhere. A link or
            // redirect back into this repo's tree must name the same snapshot.
            if let Some((root, dir)) = &pinned_dir
                && page.path() != dir
                && page.origin() == hub_origin
            {
                let candidate = decoded_segments(page.path());
                if candidate.starts_with(&decoded_segments(root.path())) && candidate != decoded_segments(dir) {
                    return Err(Error::hub("tree listing left the pinned commit or directory"));
                }
            }
            Ok(())
        };
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
            check_pin(resp.url())?;
            let next = pagination::next_page(&resp);
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
            check_pin(&next)?;
            visited.insert(url);
            if visited.contains(&next) {
                return Err(Error::hub("tree listing repeated a pagination link"));
            }
            url = next;
        }
    }
}

fn parse_tree_url(url: &str) -> Result<Url> {
    Url::parse(url).map_err(|e| Error::hub(format!("invalid tree listing URL: {e}")))
}

// Split before decoding so an encoded slash stays inside one segment.
// Url already resolves dot segments when parsing links and redirects.
fn decoded_segments(path: &str) -> Vec<Cow<'_, [u8]>> {
    path.trim_end_matches('/')
        .split('/')
        .map(|part| percent_decode_str(part).into())
        .collect()
}

#[cfg(test)]
mod tests;
