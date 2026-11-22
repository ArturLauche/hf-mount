use super::*;
use crate::hub_api::TreeEntry;
use crate::virtual_fs::{NEG_CACHE_CAPACITY, VirtualFs};

/// Reconcile one new file in each of 512 loaded directories while most
/// cached misses belong to unaffected directories. Setup is outside the timer.
#[test]
#[ignore = "release-mode poll reconciliation benchmark"]
fn bench_poll_many_changed_directories() {
    let mut table = InodeTable::new(false);
    let mut dirs = Vec::new();
    let mut entries = Vec::new();
    let mut polled = HashSet::new();
    for n in 0..512 {
        let path = format!("dir-{n}");
        dirs.push(table.insert(
            ROOT_INODE,
            path.clone(),
            path.clone(),
            InodeKind::Directory,
            0,
            SystemTime::UNIX_EPOCH,
            None,
            0o755,
            1000,
            1000,
        ));
        entries.push(TreeEntry {
            path: format!("{path}/added"),
            entry_type: "file".into(),
            size: Some(1),
            xet_hash: None,
            oid: None,
            mtime: None,
        });
        polled.insert(path);
    }
    let inodes = Arc::new(RwLock::new(table));
    let negative = Arc::new(RwLock::new(HashMap::new()));
    let invalidator = Invalidator::default();
    let mut timings = Vec::new();
    for _ in 0..11 {
        {
            let mut table = inodes.write().unwrap();
            for dir in &dirs {
                table.get_mut(*dir).unwrap().children_loaded_at = Some(Instant::now());
            }
            let mut cache = negative.write().unwrap();
            for n in 0..NEG_CACHE_CAPACITY {
                let path = if n % 4 == 0 {
                    format!("dir-{}/missing", n / 4)
                } else {
                    format!("unaffected/{n}")
                };
                cache.insert(path, Instant::now());
            }
        }

        let remote = entries.clone();
        let start = Instant::now();
        VirtualFs::apply_poll_diff(remote, &polled, &inodes, &negative, &invalidator);
        timings.push(start.elapsed());
        assert_eq!(negative.read().unwrap().len(), NEG_CACHE_CAPACITY * 3 / 4);
        assert!(dirs.iter().all(|dir| !inodes.read().unwrap().is_children_loaded(*dir)));
    }
    timings.sort_unstable();
    eprintln!(
        "512 changed directories, {NEG_CACHE_CAPACITY} cached misses: median {:?}",
        timings[timings.len() / 2]
    );
}

#[test]
fn invalidating_misses_respects_directory_boundaries() {
    let paths = [
        "a/b",
        "a/b/c",
        "a/b/c/d",
        "a/bb",
        "a",
        "a/other",
        "é/data",
        "é/data/missing",
    ];
    let original: HashMap<_, _> = paths.iter().map(|p| (p.to_string(), Instant::now())).collect();
    for (changed, keep) in [
        (vec!["a/b"], vec!["a/bb", "a", "a/other", "é/data", "é/data/missing"]),
        (vec!["a/b", "é/data"], vec!["a/bb", "a", "a/other"]),
        (vec!["", "a"], vec![]),
    ] {
        let mut cache = original.clone();
        invalidate_cached_misses(&mut cache, &changed.into_iter().collect());
        assert_eq!(
            cache.keys().map(String::as_str).collect::<HashSet<_>>(),
            keep.into_iter().collect()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn incomplete_rounds_back_off_and_retry_the_same_revision() {
    use crate::test_mocks::MockHub;

    async fn advance(secs: u64) {
        tokio::time::advance(Duration::from_secs(secs)).await;
        tokio::task::yield_now().await;
    }

    let hub = MockHub::new_repo();
    let mut table = InodeTable::new(false);
    let file = table.insert(
        ROOT_INODE,
        "file".into(),
        "file".into(),
        InodeKind::File,
        1,
        SystemTime::UNIX_EPOCH,
        Some("old".into()),
        0o644,
        1000,
        1000,
    );
    let inodes = Arc::new(RwLock::new(table));
    let poll = tokio::spawn(VirtualFs::poll_remote_changes(
        hub.clone(),
        inodes.clone(),
        Default::default(),
        Default::default(),
        Some(Duration::from_secs(1)),
        1,
        false,
    ));
    tokio::task::yield_now().await;
    assert_eq!(hub.probe_revision_call_count(), 0);

    // An empty cache needs no probe. Once root loads, a real listing must
    // still complete before the commit can be checkpointed.
    advance(1).await;
    assert_eq!(hub.list_tree_call_count(), 0);
    assert_eq!(hub.probe_revision_call_count(), 0, "no directories to reconcile yet");
    inodes.write().unwrap().get_mut(ROOT_INODE).unwrap().children_loaded_at = Some(Instant::now());
    hub.add_file("file", 2, Some("new"), None);
    hub.fail_next_list_tree_with_status(503);
    advance(1).await;
    assert_eq!(hub.list_tree_call_count(), 1);
    hub.fail_next_list_tree_with_status(429);
    advance(1).await;
    assert_eq!(
        hub.list_tree_call_count(),
        1,
        "first failure delays the retry for two intervals"
    );
    advance(1).await;
    assert_eq!(hub.list_tree_call_count(), 2);
    advance(3).await;
    assert_eq!(
        hub.list_tree_call_count(),
        2,
        "second failure delays the retry for four intervals"
    );
    advance(1).await;
    assert_eq!(hub.list_tree_call_count(), 3);
    assert_eq!(inodes.read().unwrap().get(file).unwrap().size, 2);

    let probes = hub.probe_revision_call_count();
    advance(1).await;
    assert_eq!(
        hub.probe_revision_call_count(),
        probes + 1,
        "successful reconcile resets the interval"
    );
    assert_eq!(hub.list_tree_call_count(), 3, "completed revision now skips listings");
    // Only a child listing remains loaded. It came from the mutable ref
    // after the checkpoint, and needs a pinned poll even at the same SHA.
    let nested = {
        let mut table = inodes.write().unwrap();
        table.invalidate_children(ROOT_INODE);
        let dir = table.insert(
            ROOT_INODE,
            "dir".into(),
            "dir".into(),
            InodeKind::Directory,
            0,
            SystemTime::UNIX_EPOCH,
            None,
            0o755,
            1000,
            1000,
        );
        let nested = table.insert(
            dir,
            "child".into(),
            "dir/child".into(),
            InodeKind::File,
            9,
            SystemTime::UNIX_EPOCH,
            None,
            0o644,
            1000,
            1000,
        );
        table.finish_remote_listing(dir, false);
        nested
    };
    hub.add_file("dir/child", 5, Some("pinned"), None);
    advance(1).await;
    assert_eq!(hub.list_tree_call_count(), 4);
    assert_eq!(inodes.read().unwrap().get(nested).unwrap().size, 5);
    advance(1).await;
    assert_eq!(hub.list_tree_call_count(), 4);
    poll.abort();
    assert!(poll.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn repo_poll_lists_the_probed_commit_while_the_mounted_ref_moves() {
    use crate::hub_api::{HubApiClient, RepoType, SourceKind};
    use crate::test_mocks::serve_hub_requests;

    let mut head = "commit-a";
    let mut probes = 0;
    let (endpoint, requests) = serve_hub_requests(6, move |target| {
        if target.contains("/revision/") {
            let info = format!(r#"{{"id":"org/data","sha":"{head}"}}"#);
            probes += 1;
            if probes == 2 {
                head = "commit-b"; // After the poll probe and before either tree page.
            }
            return (reqwest::StatusCode::OK, String::new(), info);
        }
        let commit = if target.contains("/tree/commit-a?") {
            "commit-a"
        } else {
            head
        };
        let second_page = target.ends_with("?cursor=2");
        let (headers, path) = if second_page {
            head = "commit-a"; // The next probe will see A again.
            ("", "two")
        } else {
            ("Link: <?cursor=2>; rel=next\r\n", "one")
        };
        (
            reqwest::StatusCode::OK,
            headers.into(),
            format!(r#"[{{"path":"{path}","type":"file","size":1,"oid":"{commit}"}}]"#),
        )
    })
    .await;

    let client = HubApiClient::from_source(
        &endpoint,
        None,
        None,
        SourceKind::Repo {
            repo_type: RepoType::Dataset,
            repo_id: "org/data".into(),
            revision: "feature/docs".into(),
        },
        String::new(),
        "test",
    )
    .await
    .unwrap();
    let mut table = InodeTable::new(false);
    for path in ["one", "two"] {
        table.insert(
            ROOT_INODE,
            path.into(),
            path.into(),
            InodeKind::File,
            1,
            SystemTime::UNIX_EPOCH,
            None,
            0o644,
            1000,
            1000,
        );
    }
    table.get_mut(ROOT_INODE).unwrap().children_loaded_at = Some(Instant::now());
    let inodes = Arc::new(RwLock::new(table));
    let poll = tokio::spawn(VirtualFs::poll_remote_changes(
        client,
        inodes.clone(),
        Default::default(),
        Default::default(),
        Some(Duration::from_millis(5)),
        1,
        false,
    ));
    let requests = requests.await.unwrap();
    poll.abort();
    assert!(poll.await.unwrap_err().is_cancelled());

    let info_url = "/api/datasets/org/data/revision/feature%2Fdocs?expand=sha&expand=lastModified";
    let paths: Vec<_> = requests
        .iter()
        .map(|r| r.split_ascii_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            info_url,
            info_url,
            "/api/datasets/org/data/tree/commit-a?limit=1000",
            "/api/datasets/org/data/tree/commit-a?cursor=2",
            info_url,
            info_url,
        ]
    );
    let table = inodes.read().unwrap();
    for path in ["one", "two"] {
        assert_eq!(table.get_by_path(path).unwrap().etag.as_deref(), Some("commit-a"));
    }
}

#[tokio::test]
async fn legacy_repo_skips_unchanged_trees_and_detects_commits_with_identical_timestamps() {
    use crate::hub_api::{HubApiClient, RepoType, SourceKind};
    use crate::test_mocks::serve_hub_responses;

    let metadata = r#"{"id":"org/model","lastModified":"2025-01-01T00:00:00Z"}"#;
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let head_a = format!(r#"[{{"id":"{a}"}}]"#);
    let head_b = format!(r#"[{{"id":"{b}"}}]"#);
    let (url, requests) = serve_hub_responses(&[
        ("", metadata),
        ("", metadata),
        ("", &head_a),
        ("", "[]"),
        ("", &head_a),
        ("", &head_b),
        ("", "[]"),
        ("", &head_b),
    ])
    .await;
    let client = HubApiClient::from_source(
        &url,
        Some("test"),
        None,
        SourceKind::Repo {
            repo_id: "org/model".into(),
            repo_type: RepoType::Model,
            revision: "feature/docs".into(),
        },
        String::new(),
        "test",
    )
    .await
    .unwrap();
    let mut table = InodeTable::new(false);
    table.get_mut(ROOT_INODE).unwrap().children_loaded_at = Some(Instant::now());
    let poll = tokio::spawn(VirtualFs::poll_remote_changes(
        client,
        Arc::new(RwLock::new(table)),
        Default::default(),
        Default::default(),
        Some(Duration::from_millis(5)),
        1,
        false,
    ));
    let requests = requests.await.unwrap();
    poll.abort();
    assert!(poll.await.unwrap_err().is_cancelled());
    let info = "/api/models/org/model/revision/feature%2Fdocs?expand=sha&expand=lastModified";
    let history = "/api/models/org/model/commits/feature%2Fdocs?limit=1";
    let paths: Vec<_> = requests
        .iter()
        .map(|r| r.split_ascii_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            info,
            info,
            history,
            &format!("/api/models/org/model/tree/{a}?limit=1000"),
            history,
            history,
            &format!("/api/models/org/model/tree/{b}?limit=1000"),
            history
        ]
    );
    assert!(
        requests
            .iter()
            .all(|r| r.to_ascii_lowercase().contains("authorization: bearer test\r\n"))
    );
}
