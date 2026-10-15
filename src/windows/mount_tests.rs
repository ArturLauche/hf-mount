//! Real WinFsp drive-letter tests.
//!
//! Each test mounts a `VirtualFs` backed by the in-memory mocks on a free
//! drive letter and drives it through Win32 (Rust std, `cmd`, PowerShell).
//! They need the WinFsp driver, so they are `#[ignore]`d by default; CI runs
//! them with `cargo test --lib -- --ignored windows::mount_tests` after
//! installing WinFsp. A run without WinFsp fails loudly instead of skipping.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hub_api::BatchOp;
use crate::test_mocks::{MockHub, MockXet, TestOpts, generated_byte, make_test_vfs};
use crate::virtual_fs::VirtualFs;
use crate::windows::drives;
use crate::windows::winfsp_fs::{MountConfig, MountError, WinFspMount};

/// Tests share the drive-letter namespace; run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PATH_NOT_FOUND: i32 = 3;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_WRITE_PROTECT: i32 = 19;

struct TestMount {
    mount: Option<WinFspMount>,
    vfs: Arc<VirtualFs>,
    root: PathBuf,
    letter: String,
    _rt: tokio::runtime::Runtime,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl TestMount {
    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn unmount(&mut self) {
        self.mount.take();
    }
}

impl Drop for TestMount {
    fn drop(&mut self) {
        self.mount.take();
        self.vfs.shutdown();
    }
}

fn free_letter() -> String {
    let letter = drives::free_letters().pop().expect("no free drive letter");
    format!("{letter}:")
}

fn config(letter: &str, read_only: bool) -> MountConfig {
    MountConfig {
        mount_point: letter.to_string(),
        volume_label: "hf-mount test".to_string(),
        read_only,
        case_insensitive_fallback: read_only,
        file_info_timeout: Duration::from_millis(500),
        dispatcher_threads: 4,
        flush_wait_timeout: Duration::from_secs(20),
        volume_serial: 0x4846_4d54,
        debug_log: false,
    }
}

fn mount(hub: Arc<MockHub>, xet: Arc<MockXet>, opts: TestOpts) -> TestMount {
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    crate::windows::winfsp::api().expect("WinFsp must be installed to run the mount tests");
    let read_only = opts.read_only;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let vfs = make_test_vfs(hub, xet, opts, &rt);
    let letter = free_letter();
    let mount = WinFspMount::mount(vfs.clone(), rt.handle().clone(), config(&letter, read_only))
        .unwrap_or_else(|e| panic!("mount at {letter} failed: {e}"));
    let root = PathBuf::from(format!("{letter}\\"));
    assert!(root.exists(), "{letter} not visible after mount");
    TestMount {
        mount: Some(mount),
        vfs,
        root,
        letter,
        _rt: rt,
        _serial: serial,
    }
}

fn repo_fixture() -> (Arc<MockHub>, Arc<MockXet>) {
    let hub = MockHub::new_repo();
    let xet = MockXet::new();
    let files: &[(&str, &str, &[u8])] = &[
        ("README.md", "h_readme", b"# hf-mount\nhello from the Hub\n"),
        ("config.json", "h_config", br#"{"model_type": "gpt2", "n_layer": 12}"#),
        ("data/train.txt", "h_train", b"line 1\nline 2\n"),
        ("data/nested/deep.txt", "h_deep", b"deep"),
        (
            "donn\u{e9}es/\u{65e5}\u{672c}\u{8a9e}.txt",
            "h_jp",
            "こんにちは".as_bytes(),
        ),
        ("emoji \u{1F600}.md", "h_emoji", b"smile"),
        ("Dup.txt", "h_dup_upper", b"UPPER"),
        ("dup.txt", "h_dup_lower", b"lower"),
        // Names Windows cannot represent: must be hidden, not aliased.
        ("bad:name.txt", "h_bad1", b"x"),
        ("trailing.", "h_bad2", b"x"),
        ("CON", "h_bad3", b"x"),
    ];
    for (path, hash, content) in files {
        hub.add_file(path, content.len() as u64, Some(hash), None);
        xet.add_file(hash, content);
    }
    hub.add_dir("data");
    hub.add_dir("data/nested");
    hub.add_dir("donn\u{e9}es");
    (hub, xet)
}

fn read_only_opts() -> TestOpts {
    TestOpts {
        read_only: true,
        serve_lookup_from_cache: true,
        ..Default::default()
    }
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn shell(program: &str, args: &[&str]) -> String {
    let out = Command::new(program).args(args).output().expect("spawn shell");
    assert!(
        out.status.success(),
        "{program} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn repo_enumerates_and_reads_exact_bytes() {
    let (hub, xet) = repo_fixture();
    let t = mount(hub, xet, read_only_opts());

    assert_eq!(
        sorted_names(&t.root),
        vec![
            "Dup.txt",
            "README.md",
            "config.json",
            "data",
            "donn\u{e9}es",
            "dup.txt",
            "emoji \u{1F600}.md"
        ]
    );
    assert_eq!(sorted_names(&t.path("data")), vec!["nested", "train.txt"]);
    assert_eq!(
        std::fs::read(t.path("README.md")).unwrap(),
        b"# hf-mount\nhello from the Hub\n"
    );
    assert_eq!(std::fs::read(t.path("data\\nested\\deep.txt")).unwrap(), b"deep");

    let meta = std::fs::metadata(t.path("config.json")).unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.len(), br#"{"model_type": "gpt2", "n_layer": 12}"#.len() as u64);
    assert!(meta.permissions().readonly());
    assert!(std::fs::metadata(t.path("data")).unwrap().is_dir());

    // Other processes in the same session see the volume too.
    let cmd = shell("cmd", &["/C", "dir", "/B", &format!("{}\\", t.letter)]);
    for name in ["README.md", "config.json", "data"] {
        assert!(cmd.lines().any(|l| l.trim() == name), "cmd dir missing {name}: {cmd}");
    }
    // ...and read it: certutil (in System32 on every Windows) hashes the file
    // through its own handle. Its output is "SHA256 hash of <path>:", the hex
    // digest, then a status line.
    let readme = t.path("README.md");
    let certutil = shell("certutil", &["-hashfile", &readme.display().to_string(), "SHA256"]);
    let digest = certutil
        .lines()
        .nth(1)
        .unwrap_or_default()
        .replace(' ', "")
        .to_ascii_uppercase();
    assert_eq!(digest, sha256_hex(b"# hf-mount\nhello from the Hub\n"), "certutil said: {certutil}");
}

/// Minimal SHA-256 for the cross-process check (avoids a dev-dependency).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08X}")).collect()
}

#[test]
fn sha256_helper_matches_known_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
    );
    assert_eq!(
        sha256_hex(b""),
        "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
    );
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn large_file_offsets_beyond_4gib_random_and_overlapping_reads() {
    const SIZE: u64 = 5 * 1024 * 1024 * 1024 + 12_345;
    let hub = MockHub::new_repo();
    let xet = MockXet::new();
    hub.add_file("big.bin", SIZE, Some("h_big"), None);
    xet.add_generated_file("h_big", SIZE);
    let t = mount(hub, xet, read_only_opts());

    assert_eq!(std::fs::metadata(t.path("big.bin")).unwrap().len(), SIZE);
    let mut f = std::fs::File::open(t.path("big.bin")).unwrap();
    let check = |f: &mut std::fs::File, offset: u64, len: usize| {
        f.seek(SeekFrom::Start(offset)).unwrap();
        let mut buf = vec![0u8; len];
        let n = f.read(&mut buf).unwrap();
        let expected = len.min((SIZE - offset) as usize);
        assert_eq!(n, expected, "short read at {offset}");
        for (i, b) in buf[..n].iter().enumerate() {
            assert_eq!(
                *b,
                generated_byte(offset + i as u64),
                "byte {} differs",
                offset + i as u64
            );
        }
    };
    // Past 4 GiB, straddling the 4 GiB boundary, and the tail.
    check(&mut f, 4 * 1024 * 1024 * 1024 + 123, 65_536);
    check(&mut f, 4 * 1024 * 1024 * 1024 - 1000, 4096);
    check(&mut f, SIZE - 100, 4096);
    // Deterministic pseudo-random seeks, including overlapping ranges.
    let mut x: u64 = 0x1234_5678_9abc_def0;
    for _ in 0..24 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let offset = x % (SIZE - 1);
        check(&mut f, offset, 8192);
        check(&mut f, offset + 4000, 8192);
    }
    // Reading at EOF returns 0 bytes, not an error.
    f.seek(SeekFrom::Start(SIZE)).unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(f.read(&mut buf).unwrap(), 0);
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn concurrent_handles_and_close_orders() {
    const SIZE: u64 = 32 * 1024 * 1024;
    let hub = MockHub::new_repo();
    let xet = MockXet::new();
    hub.add_file("shared.bin", SIZE, Some("h_shared"), None);
    xet.add_generated_file("h_shared", SIZE);
    let t = mount(hub, xet, read_only_opts());
    let path = t.path("shared.bin");

    std::thread::scope(|s| {
        for worker in 0..8u64 {
            let path = path.clone();
            s.spawn(move || {
                let mut f = std::fs::File::open(&path).unwrap();
                for round in 0..16u64 {
                    let offset = (worker * 3_000_017 + round * 1_048_583) % (SIZE - 70_000);
                    f.seek(SeekFrom::Start(offset)).unwrap();
                    let mut buf = vec![0u8; 65_536];
                    f.read_exact(&mut buf).unwrap();
                    assert!(
                        buf.iter()
                            .enumerate()
                            .all(|(i, b)| *b == generated_byte(offset + i as u64))
                    );
                }
            });
        }
    });

    // Interleaved opens and closes in different orders.
    let a = std::fs::File::open(&path).unwrap();
    let mut b = std::fs::File::open(&path).unwrap();
    let c = std::fs::File::open(&path).unwrap();
    drop(a);
    let mut buf = [0u8; 4];
    b.read_exact(&mut buf).unwrap();
    assert_eq!(
        buf,
        [
            generated_byte(0),
            generated_byte(1),
            generated_byte(2),
            generated_byte(3)
        ]
    );
    drop(c);
    drop(b);
    // Reopen after all handles closed.
    let mut again = std::fs::File::open(&path).unwrap();
    again.read_exact(&mut buf).unwrap();
    assert_eq!(buf[0], generated_byte(0));
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn unicode_names_and_unrepresentable_names_hidden() {
    let (hub, xet) = repo_fixture();
    let t = mount(hub, xet, read_only_opts());
    assert_eq!(
        std::fs::read_to_string(t.path("donn\u{e9}es\\\u{65e5}\u{672c}\u{8a9e}.txt")).unwrap(),
        "こんにちは"
    );
    assert_eq!(std::fs::read(t.path("emoji \u{1F600}.md")).unwrap(), b"smile");
    let names = sorted_names(&t.root);
    for hidden in ["bad:name.txt", "trailing.", "CON"] {
        assert!(!names.iter().any(|n| n == hidden), "{hidden} should be hidden");
    }
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn not_found_errors_are_accurate() {
    let (hub, xet) = repo_fixture();
    let t = mount(hub, xet, read_only_opts());
    let err = std::fs::metadata(t.path("missing.txt")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(ERROR_FILE_NOT_FOUND), "{err:?}");
    let err = std::fs::metadata(t.path("missing-dir\\file.txt")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(ERROR_PATH_NOT_FOUND), "{err:?}");
    let err = std::fs::read_dir(t.path("README.md\\child")).unwrap_err();
    assert!(
        matches!(err.raw_os_error(), Some(ERROR_PATH_NOT_FOUND) | Some(267)),
        "{err:?}"
    );
    // Shell probes on a listed directory never hit the network miss path and
    // still answer "not found".
    assert!(!t.path("desktop.ini").exists());
    assert!(!t.path("data\\desktop.ini").exists());
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn read_only_mount_refuses_writes() {
    let (hub, xet) = repo_fixture();
    let hub2 = hub.clone();
    let t = mount(hub, xet, read_only_opts());

    let assert_refused = |err: std::io::Error| {
        assert!(
            matches!(
                err.raw_os_error(),
                Some(ERROR_WRITE_PROTECT) | Some(ERROR_ACCESS_DENIED)
            ),
            "unexpected error {err:?}"
        );
    };
    assert_refused(std::fs::File::create(t.path("new.txt")).unwrap_err());
    assert_refused(
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path("README.md"))
            .unwrap_err(),
    );
    assert_refused(std::fs::remove_file(t.path("README.md")).unwrap_err());
    assert_refused(std::fs::create_dir(t.path("newdir")).unwrap_err());
    assert_refused(std::fs::rename(t.path("README.md"), t.path("renamed.md")).unwrap_err());
    assert_eq!(
        std::fs::read(t.path("README.md")).unwrap(),
        b"# hf-mount\nhello from the Hub\n"
    );
    assert!(
        hub2.take_batch_log().is_empty(),
        "read-only mount sent remote mutations"
    );
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn case_insensitive_lookup_only_when_unambiguous() {
    let (hub, xet) = repo_fixture();
    let t = mount(hub, xet, read_only_opts());
    // Unique match: typed with different case.
    assert_eq!(
        std::fs::read(t.path("readme.MD")).unwrap(),
        b"# hf-mount\nhello from the Hub\n"
    );
    assert_eq!(std::fs::read(t.path("DATA\\Train.TXT")).unwrap(), b"line 1\nline 2\n");
    // Two remote files differing only in case keep their identity...
    assert_eq!(std::fs::read(t.path("Dup.txt")).unwrap(), b"UPPER");
    assert_eq!(std::fs::read(t.path("dup.txt")).unwrap(), b"lower");
    // ...and an ambiguous spelling resolves to neither.
    let err = std::fs::read(t.path("DUP.TXT")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(ERROR_FILE_NOT_FOUND));
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn unmount_releases_letter_and_remount_works() {
    let (hub, xet) = repo_fixture();
    let mut t = mount(hub.clone(), xet.clone(), read_only_opts());
    let letter = t.letter.clone();
    let open = std::fs::File::open(t.path("README.md")).unwrap();
    t.unmount();
    drop(open);
    let ch = letter.chars().next().unwrap();
    assert!(!drives::drive_exists(ch), "{letter} still present after unmount");
    assert!(drives::is_letter_free(ch), "{letter} not released");
    drop(t);

    // Same letter can be mounted again with no stale state.
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let vfs = make_test_vfs(hub, xet, read_only_opts(), &rt);
    let mount = WinFspMount::mount(vfs.clone(), rt.handle().clone(), config(&letter, true)).unwrap();
    assert_eq!(
        std::fs::read(format!("{letter}\\README.md")).unwrap(),
        b"# hf-mount\nhello from the Hub\n"
    );

    // An occupied letter is detected before WinFsp is asked.
    let other = make_test_vfs(MockHub::new_repo(), MockXet::new(), read_only_opts(), &rt);
    match WinFspMount::mount(other.clone(), rt.handle().clone(), config(&letter, true)) {
        Err(MountError::TargetInUse(l)) => assert_eq!(l, letter),
        Err(e) => panic!("expected TargetInUse, got {e}"),
        Ok(_) => panic!("mounted twice on {letter}"),
    }
    drop(mount);
    vfs.shutdown();
    other.shutdown();
}

fn wait_clean(vfs: &VirtualFs) {
    for _ in 0..400 {
        if vfs.pending_uploads().dirty_files == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("uploads never committed: {:?}", vfs.pending_uploads());
}

fn added_paths(log: &[Vec<BatchOp>]) -> Vec<String> {
    log.iter()
        .flatten()
        .filter_map(|op| match op {
            BatchOp::AddFile { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn writable_bucket_create_modify_rename_delete() {
    let hub = MockHub::new();
    let xet = MockXet::new();
    hub.add_file("existing.txt", 8, Some("h_existing"), None);
    xet.add_file("h_existing", b"original");
    hub.reflect_commits(xet.clone());
    let t = mount(
        hub.clone(),
        xet.clone(),
        TestOpts {
            advanced_writes: true,
            ..Default::default()
        },
    );

    // Create + write + close; data readable back immediately.
    std::fs::write(t.path("notes.txt"), b"first version").unwrap();
    assert_eq!(std::fs::read(t.path("notes.txt")).unwrap(), b"first version");

    // FlushFileBuffers waits for the upload to commit.
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(t.path("notes.txt"))
            .unwrap();
        f.seek(SeekFrom::Start(6)).unwrap();
        f.write_all(b"VERSION").unwrap();
        f.sync_all().unwrap();
        // notes.txt was the only modified file; sync_all returned only after
        // its upload committed.
        assert_eq!(t.vfs.pending_uploads().dirty_files, 0);
    }
    wait_clean(&t.vfs);
    assert_eq!(std::fs::read(t.path("notes.txt")).unwrap(), b"first VERSION");
    assert!(added_paths(&hub.take_batch_log()).contains(&"notes.txt".to_string()));

    // Editor-style save: write a temp file, then rename over the original.
    std::fs::write(t.path("existing.txt.tmp"), b"replaced content").unwrap();
    std::fs::rename(t.path("existing.txt.tmp"), t.path("existing.txt")).unwrap();
    assert_eq!(std::fs::read(t.path("existing.txt")).unwrap(), b"replaced content");
    assert!(!t.path("existing.txt.tmp").exists());

    // Truncating overwrite.
    std::fs::write(t.path("existing.txt"), b"short").unwrap();
    assert_eq!(std::fs::read(t.path("existing.txt")).unwrap(), b"short");

    // Directories.
    std::fs::create_dir(t.path("folder")).unwrap();
    std::fs::write(t.path("folder\\inner.bin"), [1u8, 2, 3]).unwrap();
    let err = std::fs::remove_dir(t.path("folder")).unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(145),
        "expected ERROR_DIR_NOT_EMPTY, got {err:?}"
    );
    std::fs::remove_file(t.path("folder\\inner.bin")).unwrap();
    std::fs::remove_dir(t.path("folder")).unwrap();
    assert!(!t.path("folder").exists());

    // Invalid Windows names are refused at creation.
    assert!(std::fs::write(t.path("bad?.txt"), b"x").is_err());

    // Delete.
    std::fs::remove_file(t.path("notes.txt")).unwrap();
    assert!(!t.path("notes.txt").exists());

    wait_clean(&t.vfs);
    assert_eq!(t.vfs.pending_uploads().failed_files, 0);
    let names = sorted_names(&t.root);
    assert_eq!(names, vec!["existing.txt"]);
}

#[test]
#[ignore = "requires the WinFsp driver"]
fn opening_for_write_without_writing_does_not_dirty() {
    let hub = MockHub::new();
    let xet = MockXet::new();
    hub.add_file("big.bin", 4096, Some("h_rw"), None);
    xet.add_file("h_rw", &[7u8; 4096]);
    let t = mount(
        hub.clone(),
        xet.clone(),
        TestOpts {
            advanced_writes: true,
            ..Default::default()
        },
    );
    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(t.path("big.bin"))
            .unwrap();
        let mut buf = [0u8; 16];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [7u8; 16]);
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(t.vfs.pending_uploads().dirty_files, 0);
    assert_eq!(xet.download_to_file_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(hub.take_batch_log().is_empty());
}
