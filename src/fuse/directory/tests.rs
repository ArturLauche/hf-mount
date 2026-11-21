use std::time::{Duration, Instant};

use super::*;
use crate::test_mocks::{MockHub, MockXet, TestOpts, make_overlay_test_vfs_with_root, make_test_vfs};
use crate::virtual_fs::inode::ROOT_INODE;

fn adapter(hub: Arc<MockHub>, ttl: Duration) -> (tokio::runtime::Runtime, FuseAdapter) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let vfs = make_test_vfs(
        hub,
        MockXet::new(),
        TestOpts {
            metadata_ttl: ttl,
            ..Default::default()
        },
        &runtime,
    );
    let fuse = FuseAdapter::new(runtime.handle().clone(), vfs, ttl, false, false, false);
    (runtime, fuse)
}

fn page(fuse: &FuseAdapter, fh: FileHandle, offset: u64, limit: usize) -> VirtualFsResult<Vec<(u64, String)>> {
    let mut out = Vec::new();
    fuse.read_directory(fh, offset, |cookie, e| {
        if out.len() == limit {
            return true;
        }
        out.push((cookie, e.name.to_string()));
        false
    })?;
    Ok(out)
}

#[test]
fn continuation_and_seek_survive_changes_and_rewind_refreshes() {
    let hub = MockHub::new();
    for name in ["a", "b", "c"] {
        hub.add_file(name, 10, None, None);
    }
    let (_rt, fuse) = adapter(hub.clone(), Duration::ZERO);
    let before = hub.list_tree_call_count();
    let fh = fuse.open_directory(ROOT_INODE).unwrap();
    let other = fuse.open_directory(ROOT_INODE).unwrap();
    assert_eq!(hub.list_tree_call_count(), before, "opendir is lazy");
    assert_eq!(
        page(&fuse, fh, 0, 3).unwrap(),
        [(1, ".".into()), (2, "..".into()), (3, "a".into())]
    );

    hub.remove_file("a");
    hub.remove_file("b");
    hub.add_file("renamed", 10, None, None);
    let continuation = [(4, "b".into()), (5, "c".into())];
    assert_eq!(page(&fuse, fh, 3, 10).unwrap(), continuation);
    assert_eq!(page(&fuse, fh, 3, 10).unwrap(), continuation, "seek to a prior cookie");
    assert!(page(&fuse, fh, u64::MAX, 10).unwrap().is_empty());
    assert_eq!(hub.list_tree_call_count(), before + 1, "continuations never relist");
    let fresh = page(&fuse, other, 0, 10).unwrap();
    assert_eq!(
        fresh.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(),
        [".", "..", "c", "renamed"]
    );
    assert_eq!(page(&fuse, fh, 0, 10).unwrap(), fresh);

    fuse.close_directory(fh).unwrap();
    assert_eq!(page(&fuse, fh, 1, 10), Err(libc::EBADF));
    assert_eq!(fuse.close_directory(fh), Err(libc::EBADF));
    assert_eq!(page(&fuse, other, 2, 10).unwrap(), fresh[2..]);
    fuse.close_directory(other).unwrap();
}

#[test]
fn directory_load_error_can_be_retried_on_the_same_handle() {
    let hub = MockHub::new();
    hub.add_file("folder/item", 1, Some("hash"), None);
    let (rt, fuse) = adapter(hub.clone(), Duration::from_secs(60));
    let ino = rt.block_on(fuse.virtual_fs.lookup(ROOT_INODE, "folder")).unwrap().ino;
    let fh = fuse.open_directory(ino).unwrap();
    hub.fail_next_list_tree_with_status(429);
    assert_eq!(page(&fuse, fh, 0, 4), Err(libc::EAGAIN));
    assert_eq!(page(&fuse, fh, 2, 4).unwrap(), [(3, "item".into())]);
    let item = rt.block_on(fuse.virtual_fs.lookup(ino, "item")).unwrap().ino;
    assert_eq!(fuse.open_directory(item), Err(libc::ENOTDIR));
    assert_eq!(fuse.open_directory(u64::MAX), Err(libc::ENOENT));
    fuse.close_directory(fh).unwrap();
}

#[test]
fn pages_include_the_merged_overlay_and_keep_local_precedence() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("shared"), b"local").unwrap();
    std::fs::write(root.path().join("local-only"), b"local").unwrap();
    let hub = MockHub::new();
    hub.add_file("shared", 123, Some("remote"), None);
    hub.add_file("remote-only", 42, Some("another"), None);
    let t = make_overlay_test_vfs_with_root(hub, MockXet::new(), root.path().to_owned());
    let fuse = FuseAdapter::new(
        t.runtime.handle().clone(),
        t.vfs.clone(),
        Duration::ZERO,
        false,
        false,
        false,
    );
    let fh = fuse.open_directory(ROOT_INODE).unwrap();
    let mut names = Vec::new();
    let mut offset = 0;
    while let Some((cookie, name)) = page(&fuse, fh, offset, 1).unwrap().pop() {
        names.push(name);
        offset = cookie;
    }
    names.sort();
    assert_eq!(names, [".", "..", "local-only", "remote-only", "shared"]);
    assert_eq!(t.runtime.block_on(t.vfs.lookup(ROOT_INODE, "shared")).unwrap().size, 5);
    fuse.close_directory(fh).unwrap();
}

/// Local metadata only. Compare the previous full-list-per-reply algorithm
/// with the FUSE callback path, on the same warm VFS and with identical output.
#[test]
#[ignore = "release-mode directory paging benchmark"]
fn bench_directory_pages() {
    const FILES: usize = 10_000;
    const PAGE: usize = 64;
    let hub = MockHub::new();
    for i in 0..FILES {
        hub.add_file(&format!("file-{i:05}"), 10, None, None);
    }
    let (rt, fuse) = adapter(hub, Duration::from_secs(3600));
    for _ in 0..5 {
        let mut old_names = Vec::with_capacity(FILES + 2);
        let start = Instant::now();
        while old_names.len() < FILES + 2 {
            let entries = rt.block_on(fuse.virtual_fs.readdir(ROOT_INODE)).unwrap();
            for e in entries.into_iter().skip(old_names.len()).take(PAGE) {
                old_names.push(e.name);
            }
        }
        let full_lists = start.elapsed();
        let start = Instant::now();
        let fh = fuse.open_directory(ROOT_INODE).unwrap();
        let mut new_names = Vec::with_capacity(FILES + 2);
        let mut cookie = 0;
        while new_names.len() < FILES + 2 {
            let batch = page(&fuse, fh, cookie, PAGE).unwrap();
            cookie = batch.last().unwrap().0;
            new_names.extend(batch.into_iter().map(|(_, name)| name));
        }
        fuse.close_directory(fh).unwrap();
        let paged = start.elapsed();
        assert_eq!(old_names, new_names);
        eprintln!("{FILES} files, {PAGE} entries/reply: full lists {full_lists:?}, FUSE pages {paged:?}");
    }
}

/// Exercise the kernel's actual getdents buffers and cookies. Runs with
/// `--ignored` on a Linux host with permission to mount /dev/fuse.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a mountable /dev/fuse"]
fn kernel_directory_stream_survives_remote_mutation() {
    let hub = MockHub::new();
    let names: Vec<_> = (0..4096).map(|i| format!("entry-{i:04}")).collect();
    for name in &names {
        hub.add_file(name, 0, None, None);
    }
    let (_rt, fuse) = adapter(hub.clone(), Duration::ZERO);
    let mount = tempfile::tempdir().unwrap();
    let session = fuser::spawn_mount2(fuse, mount.path(), &fuser::Config::default()).unwrap();

    let mut stream = std::fs::read_dir(mount.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name());
    let first = stream.next().unwrap();
    hub.remove_file(first.to_str().unwrap());
    hub.add_file("added", 0, None, None);
    let mut observed = vec![first.clone()];
    observed.extend(stream);
    observed.sort();
    assert!(
        observed
            .iter()
            .map(|s| s.to_str().unwrap())
            .eq(names.iter().map(String::as_str))
    );

    let fresh: std::collections::HashSet<_> = std::fs::read_dir(mount.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(fresh.len(), names.len());
    assert!(!fresh.contains(&first));
    assert!(fresh.contains(std::ffi::OsStr::new("added")));
    session.umount_and_join().unwrap();
}
