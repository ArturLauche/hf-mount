//! Offline status workloads. Run with `cargo test --release --test status_bench -- --ignored --nocapture`.
use std::hint::black_box;
use std::io::Write;
use std::time::{Instant, UNIX_EPOCH};

use hf_mount::manager::logging::tail;
use hf_mount::virtual_fs::inode::{InodeKind, InodeTable, ROOT_INODE};

#[test]
#[ignore = "offline performance measurement"]
fn idle_status() {
    let mut inodes = InodeTable::default();
    for i in 0..50_000 {
        let name = format!("file-{i}");
        let ino = inodes.insert(
            ROOT_INODE,
            name.clone(),
            name,
            InodeKind::File,
            0,
            UNIX_EPOCH,
            None,
            0o644,
            0,
            0,
        );
        if i % 1000 == 0 {
            inodes.get_mut(ino).unwrap().set_dirty();
        }
    }
    let mut log = tempfile::NamedTempFile::new().unwrap();
    let line = format!("INFO {}\n", "a".repeat(122));
    for _ in 0..65_536 {
        log.write_all(line.as_bytes()).unwrap();
    }
    log.flush().unwrap();
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..100 {
            assert_eq!(black_box(&inodes).dirty_file_count(), 50);
        }
        let status = start.elapsed();
        let start = Instant::now();
        for _ in 0..10 {
            assert_eq!(tail(black_box(log.path()), 400).len(), 400);
        }
        eprintln!(
            "100 dirty-file counts: {status:?}, 10 tails (400 lines from 8 MiB): {:?}",
            start.elapsed()
        );
    }
}

/// Run in a stable Windows session, first without hosts and then with
/// `hf-mount supervise`. All queries are read-only.
#[cfg(windows)]
#[tokio::test]
#[ignore = "Windows status measurement"]
async fn windows_status() {
    use hf_mount::windows::{client::StatusConnections, drives, ops, pipe, security};
    use std::time::Duration;

    const READS: u32 = 20;
    fn time_reads<T>(mut read: impl FnMut() -> T) -> Duration {
        let start = Instant::now();
        for _ in 0..READS {
            black_box(read());
        }
        start.elapsed() / READS
    }

    let identity = pipe::Identity::current().unwrap();
    let mut reader = StatusConnections::default();
    let status = reader.list(&identity).await;
    assert!(!status.is_incomplete(), "{:?}", status.supervisor_error);
    eprintln!(
        "{} hosts, {} mounts",
        status.hosts.len(),
        ops::overview_from_hosts(status.hosts).mounts.len()
    );

    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..READS {
            let status = reader.list(&identity).await;
            assert!(!status.is_incomplete(), "{:?}", status.supervisor_error);
            black_box(ops::overview_from_hosts(status.hosts));
        }
        let overview = start.elapsed() / READS;
        let start = Instant::now();
        for _ in 0..READS {
            let status = reader.list(&identity).await;
            assert!(!status.is_incomplete(), "{:?}", status.supervisor_error);
            black_box(status.hosts);
        }
        let list = start.elapsed() / READS;
        let discovery = time_reads(|| pipe::list_host_pipes(&identity));
        let letters = time_reads(drives::free_letters);
        let driver = time_reads(ops::driver_status);
        let elevation = time_reads(security::is_elevated);
        eprintln!(
            "Mean per read ({READS}): overview {overview:?}, host list {list:?}, discovery {discovery:?}, free letters {letters:?}, driver {driver:?}, elevation {elevation:?}"
        );
    }
}
