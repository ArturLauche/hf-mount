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
    table.get_mut(ROOT_INODE).unwrap().children_loaded_at = Some(Instant::now());
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
    assert_eq!(hub.probe_revision_call_count(), 1);

    hub.add_file("file", 2, Some("new"), None);
    hub.set_revision("updated");
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
    poll.abort();
    assert!(poll.await.unwrap_err().is_cancelled());
}
