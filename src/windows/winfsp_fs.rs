//! WinFsp file system adapter over [`VirtualFs`].
//!
//! WinFsp is path based and calls us on its dispatcher threads; `VirtualFs`
//! is inode based and async. Each callback resolves the path through
//! `VirtualFs::lookup` (cached, TTL-revalidated) and blocks on the tokio
//! runtime for the async parts, exactly like the FUSE adapter does on its
//! worker threads. See `docs/windows-architecture.md` for the semantics.
//!
//! WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos.
//! <https://github.com/winfsp/winfsp>

use std::collections::HashSet;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use super::security::{SecurityDescriptor, current_user_sid, volume_sddl};
use super::util::{from_wide_ptr, wide};
use super::winfsp::{LoadError, WinFspApi};
use crate::virtual_fs::inode::{InodeKind, ROOT_INODE};
use crate::virtual_fs::{VirtualFs, VirtualFsAttr};
use crate::winfs::dirlist::DirSnapshot;
use crate::winfs::ffi::{
    self, BOOLEAN, DirInfo, DirInfoBuf, FileInfo, FileSystemInterface, FspFileSystem, FspFileSystemPrefix, NTSTATUS,
    PWSTR, VolumeInfo, VolumeParams, vp, win32,
};
use crate::winfs::ntstatus::*;
use crate::winfs::{file_info_for, names, time};

/// Upper bound on a path WinFsp hands us (`FSP_FSCTL_TRANSACT_PATH_SIZEMAX`).
const MAX_PATH_UNITS: usize = 1024;

/// Capacity reported to Windows. The Hub has no meaningful quota to show;
/// this matches what the FUSE backend reports for `statfs` (42 PB).
const VOLUME_BYTES: u64 = 42 * 1024 * 1024 * 1024 * 1024 * 1024;

/// How a volume is mounted.
#[derive(Debug, Clone)]
pub struct MountConfig {
    /// Drive letter, e.g. `H:`.
    pub mount_point: String,
    /// Volume label shown by File Explorer (truncated to 32 characters).
    pub volume_label: String,
    pub read_only: bool,
    /// On a miss, accept a case-insensitive match when it is unambiguous.
    pub case_insensitive_fallback: bool,
    /// How long the kernel may cache file/directory information.
    pub file_info_timeout: Duration,
    /// WinFsp dispatcher threads (0 = WinFsp default).
    pub dispatcher_threads: u32,
    /// How long `FlushFileBuffers` waits for a file's upload to commit.
    pub flush_wait_timeout: Duration,
    pub volume_serial: u32,
    /// Route WinFsp's own request/response tracing to the debug log.
    pub debug_log: bool,
}

/// Why a volume could not be mounted.
#[derive(Debug)]
pub enum MountError {
    Driver(LoadError),
    /// The WinFsp DLL loaded but the kernel driver could not be reached.
    DriverUnavailable(NtStatus),
    InvalidMountPoint(String),
    TargetInUse(String),
    Security(std::io::Error),
    WinFsp {
        operation: &'static str,
        status: NtStatus,
    },
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::Driver(e) => write!(f, "{e}"),
            MountError::DriverUnavailable(status) => write!(
                f,
                "the WinFsp driver could not be started (NTSTATUS 0x{:08X}); reinstall WinFsp or reboot",
                *status as u32
            ),
            MountError::InvalidMountPoint(mp) => {
                write!(f, "invalid mount point {mp:?}: use a drive letter such as H:")
            }
            MountError::TargetInUse(mp) => write!(f, "drive {mp} is already in use"),
            MountError::Security(e) => write!(f, "could not build the volume security descriptor: {e}"),
            MountError::WinFsp { operation, status } => {
                write!(f, "WinFsp {operation} failed (NTSTATUS 0x{:08X})", *status as u32)
            }
        }
    }
}

impl std::error::Error for MountError {}

/// Normalise a drive-letter mount point (`h`, `h:`, `H:\`) to `H:`.
pub fn normalize_drive_letter(input: &str) -> Option<String> {
    let trimmed = input.trim().trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    if !letter.is_ascii_uppercase() {
        return None;
    }
    match (chars.next(), chars.next()) {
        (None, None) | (Some(':'), None) => Some(format!("{letter}:")),
        _ => None,
    }
}

/// Live counters for one volume.
#[derive(Default)]
pub struct AdapterStats {
    open_handles: AtomicUsize,
    open_write_handles: AtomicUsize,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    failed_ops: AtomicU64,
    last_error: Mutex<Option<String>>,
}

/// Point-in-time copy of [`AdapterStats`].
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub open_handles: usize,
    /// Handles that acquired a writable `VirtualFs` handle (wrote or truncated).
    pub open_write_handles: usize,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub failed_ops: u64,
    pub last_error: Option<String>,
}

impl AdapterStats {
    fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            open_handles: self.open_handles.load(Ordering::Relaxed),
            open_write_handles: self.open_write_handles.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            failed_ops: self.failed_ops.load(Ordering::Relaxed),
            last_error: self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }
}

struct Adapter {
    api: &'static WinFspApi,
    vfs: Arc<VirtualFs>,
    rt: tokio::runtime::Handle,
    read_only: bool,
    case_fallback: bool,
    flush_wait: Duration,
    security: SecurityDescriptor,
    label: Vec<u16>,
    stats: Arc<AdapterStats>,
    stopped_abnormally: Arc<AtomicBool>,
    hidden_logged: Mutex<HashSet<String>>,
}

/// Per-open state (WinFsp `UserContext2`): one Windows file object.
struct OpenHandle {
    ino: u64,
    parent_ino: u64,
    is_dir: bool,
    path: Mutex<String>,
    /// Read-only `VirtualFs` handle, opened on the first read.
    read_fh: Mutex<Option<u64>>,
    /// Writable `VirtualFs` handle, opened on the first write/truncate so a
    /// read/write open that never writes does not download or re-upload.
    write_fh: Mutex<Option<u64>>,
    dir: Mutex<Option<Arc<DirSnapshot<FileInfo>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type NtResult<T> = Result<T, NtStatus>;

fn errno(e: i32) -> NtStatus {
    errno_to_ntstatus(e)
}

impl Adapter {
    fn attr(&self, ino: u64) -> NtResult<VirtualFsAttr> {
        self.vfs.getattr(ino).map_err(errno)
    }

    fn info(&self, ino: u64) -> NtResult<FileInfo> {
        Ok(file_info_for(&self.attr(ino)?, self.read_only))
    }

    fn note_hidden(&self, name: &str) {
        let mut logged = lock(&self.hidden_logged);
        if logged.len() < 1000 && logged.insert(name.to_string()) {
            let reason = names::windows_name_problem(name)
                .map(|p| p.to_string())
                .unwrap_or_default();
            info!("Hiding remote entry {name:?} from Windows listings: {reason}");
        }
    }

    /// Resolve one child of `parent`, applying the Windows visibility rules.
    /// Returns the attributes and the remote name actually matched (which
    /// differs from `name` after a case-insensitive fallback).
    fn lookup_child(&self, parent: u64, name: &str) -> NtResult<(VirtualFsAttr, String)> {
        if names::windows_name_problem(name).is_some() {
            return Err(STATUS_OBJECT_NAME_NOT_FOUND);
        }
        let found = if names::is_shell_probe_name(name) {
            // Answer File Explorer's per-folder probes from the listing only.
            self.lookup_listed(parent, |n| n == name)?
        } else {
            match self.rt.block_on(self.vfs.lookup(parent, name)) {
                Ok(attr) => Some((attr, name.to_string())),
                Err(libc::ENOENT) => None,
                Err(e) => return Err(errno(e)),
            }
        };
        let found = match found {
            Some(hit) => Some(hit),
            None if self.case_fallback => self.lookup_case_insensitive(parent, name)?,
            None => None,
        };
        match found {
            // Symlinks (creatable on Linux bucket mounts) are not surfaced:
            // there is no reparse-point support in this adapter.
            Some((attr, _)) if attr.kind == InodeKind::Symlink => Err(STATUS_OBJECT_NAME_NOT_FOUND),
            Some(hit) => Ok(hit),
            None => Err(STATUS_OBJECT_NAME_NOT_FOUND),
        }
    }

    fn lookup_listed(&self, parent: u64, matches: impl Fn(&str) -> bool) -> NtResult<Option<(VirtualFsAttr, String)>> {
        let entries = self.rt.block_on(self.vfs.readdir(parent)).map_err(errno)?;
        for entry in entries {
            if entry.name == "." || entry.name == ".." || !matches(&entry.name) {
                continue;
            }
            return Ok(Some((self.attr(entry.ino)?, entry.name)));
        }
        Ok(None)
    }

    fn lookup_case_insensitive(&self, parent: u64, name: &str) -> NtResult<Option<(VirtualFsAttr, String)>> {
        let entries = self.rt.block_on(self.vfs.readdir(parent)).map_err(errno)?;
        let candidates = entries
            .iter()
            .map(|e| e.name.as_str())
            .filter(|n| *n != "." && *n != ".." && names::is_representable(n));
        let Some(actual) = names::unique_case_insensitive_match(name, candidates).map(str::to_string) else {
            return Ok(None);
        };
        let entry = entries
            .iter()
            .find(|e| e.name == actual)
            .ok_or(STATUS_OBJECT_NAME_NOT_FOUND)?;
        Ok(Some((self.attr(entry.ino)?, actual)))
    }

    /// Resolve path components to (attributes, parent inode).
    fn resolve_components(&self, components: &[&str]) -> NtResult<(VirtualFsAttr, u64)> {
        let mut attr = self.attr(ROOT_INODE)?;
        let mut parent = ROOT_INODE;
        for (i, component) in components.iter().enumerate() {
            if attr.kind != InodeKind::Directory {
                return Err(STATUS_OBJECT_PATH_NOT_FOUND);
            }
            parent = attr.ino;
            attr = match self.lookup_child(attr.ino, component) {
                Ok((a, _)) => a,
                Err(STATUS_OBJECT_NAME_NOT_FOUND) if i + 1 < components.len() => {
                    return Err(STATUS_OBJECT_PATH_NOT_FOUND);
                }
                Err(e) => return Err(e),
            };
        }
        Ok((attr, parent))
    }

    fn resolve(&self, path: &str) -> NtResult<(VirtualFsAttr, u64)> {
        let components = names::split_path(path).map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
        self.resolve_components(&components)
    }

    /// Resolve the parent directory of `path` and return it with the leaf name.
    fn resolve_parent<'p>(&self, path: &'p str) -> NtResult<(VirtualFsAttr, &'p str)> {
        let mut components = names::split_path(path).map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
        let leaf = components.pop().ok_or(STATUS_ACCESS_DENIED)?; // the root itself
        let (parent, _) = self.resolve_components(&components).map_err(|e| {
            if e == STATUS_OBJECT_NAME_NOT_FOUND {
                STATUS_OBJECT_PATH_NOT_FOUND
            } else {
                e
            }
        })?;
        if parent.kind != InodeKind::Directory {
            return Err(STATUS_OBJECT_PATH_NOT_FOUND);
        }
        Ok((parent, leaf))
    }

    fn copy_security(&self, sd: *mut c_void, psize: *mut usize) -> NTSTATUS {
        if psize.is_null() {
            return STATUS_SUCCESS;
        }
        let bytes = self.security.as_bytes();
        // SAFETY: WinFsp passes a valid size pointer and a buffer of that size.
        unsafe {
            if *psize < bytes.len() {
                *psize = bytes.len();
                return STATUS_BUFFER_OVERFLOW;
            }
            if !sd.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), sd.cast::<u8>(), bytes.len());
            }
            *psize = bytes.len();
        }
        STATUS_SUCCESS
    }

    /// The handle to read through: the writable one once acquired (it sees
    /// this handle's writes), otherwise a lazily opened read-only one.
    fn reader(&self, h: &OpenHandle) -> NtResult<u64> {
        if let Some(fh) = *lock(&h.write_fh) {
            return Ok(fh);
        }
        let mut read_fh = lock(&h.read_fh);
        if let Some(fh) = *read_fh {
            return Ok(fh);
        }
        let fh = self
            .rt
            .block_on(self.vfs.open(h.ino, false, false, None))
            .map_err(errno)?;
        *read_fh = Some(fh);
        Ok(fh)
    }

    /// The writable handle, opened (copy-on-write into staging) on first use.
    fn writer(&self, h: &OpenHandle) -> NtResult<u64> {
        let mut write_fh = lock(&h.write_fh);
        if let Some(fh) = *write_fh {
            return Ok(fh);
        }
        let fh = self
            .rt
            .block_on(self.vfs.open(h.ino, true, false, None))
            .map_err(errno)?;
        *write_fh = Some(fh);
        self.stats.open_write_handles.fetch_add(1, Ordering::Relaxed);
        Ok(fh)
    }

    /// Wait until `ino` (or every file and queued deletion when `None`) has
    /// been committed to the Hub, a background upload fails, or the timeout
    /// expires.
    fn wait_committed(&self, ino: Option<u64>) -> NTSTATUS {
        let deadline = Instant::now() + self.flush_wait;
        match ino {
            Some(ino) => self.vfs.schedule_flush(ino),
            None => {
                self.vfs.requeue_dirty();
            }
        }
        loop {
            let dirty = match ino {
                Some(ino) => {
                    if let Some(err) = self.vfs.take_flush_error(ino) {
                        warn!("upload of ino={ino} failed: {err}");
                        return STATUS_UNEXPECTED_IO_ERROR;
                    }
                    self.vfs.is_dirty(ino)
                }
                None => {
                    let pending = self.vfs.pending_uploads();
                    pending.dirty_files > 0 || pending.pending_deletes > 0
                }
            };
            if !dirty {
                return STATUS_SUCCESS;
            }
            if Instant::now() >= deadline {
                warn!("flush: upload still pending after {:?}", self.flush_wait);
                return STATUS_IO_TIMEOUT;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn record_failure(&self, op: &str, status: NtStatus) {
        // Expected outcomes of normal use are not failures.
        if matches!(
            status,
            STATUS_OBJECT_NAME_NOT_FOUND
                | STATUS_OBJECT_PATH_NOT_FOUND
                | STATUS_OBJECT_NAME_COLLISION
                | STATUS_OBJECT_NAME_INVALID
                | STATUS_END_OF_FILE
                | STATUS_BUFFER_OVERFLOW
                | STATUS_MEDIA_WRITE_PROTECTED
                | STATUS_DIRECTORY_NOT_EMPTY
                | STATUS_NOT_A_DIRECTORY
                | STATUS_FILE_IS_A_DIRECTORY
                | STATUS_ACCESS_DENIED
        ) {
            return;
        }
        self.stats.failed_ops.fetch_add(1, Ordering::Relaxed);
        let message = format!("{op} failed with NTSTATUS 0x{:08X}", status as u32);
        debug!("{message}");
        *lock(&self.stats.last_error) = Some(message);
    }
}

// ── Callback plumbing ────────────────────────────────────────────────

/// # Safety
/// `fs` must be a file system created by [`WinFspMount::mount`].
unsafe fn adapter<'a>(fs: *mut FspFileSystem) -> &'a Adapter {
    // SAFETY: UserContext was set to a live `Adapter` before the dispatcher started.
    unsafe { &*((*fs.cast::<FspFileSystemPrefix>()).user_context as *const Adapter) }
}

/// # Safety
/// `ctx` must be a context returned from our `Open`/`Create`.
unsafe fn handle<'a>(ctx: *mut c_void) -> &'a OpenHandle {
    unsafe { &*(ctx as *const OpenHandle) }
}

/// Run a callback body, turning panics into an I/O error (unwinding into
/// WinFsp's C dispatcher would abort the process) and recording failures.
fn guarded(fs: *mut FspFileSystem, op: &'static str, f: impl FnOnce(&Adapter) -> NtResult<()>) -> NTSTATUS {
    // SAFETY: called only from WinFsp callbacks for our file systems.
    let a = unsafe { adapter(fs) };
    let status = match catch_unwind(AssertUnwindSafe(|| f(a))) {
        Ok(Ok(())) => STATUS_SUCCESS,
        Ok(Err(status)) => status,
        Err(_) => {
            error!("panic in WinFsp {op} handler");
            STATUS_UNEXPECTED_IO_ERROR
        }
    };
    if !nt_success(status) {
        a.record_failure(op, status);
    }
    status
}

unsafe fn path_arg(p: PWSTR) -> NtResult<String> {
    unsafe { from_wide_ptr(p, MAX_PATH_UNITS) }.ok_or(STATUS_OBJECT_NAME_INVALID)
}

unsafe extern "system" fn get_volume_info(fs: *mut FspFileSystem, vi: *mut VolumeInfo) -> NTSTATUS {
    guarded(fs, "GetVolumeInfo", |a| {
        // SAFETY: WinFsp passes a valid out pointer.
        let vi = unsafe { &mut *vi };
        vi.total_size = VOLUME_BYTES;
        vi.free_size = VOLUME_BYTES;
        let n = a.label.len().min(32);
        vi.volume_label[..n].copy_from_slice(&a.label[..n]);
        vi.volume_label_length = (n * 2) as u16;
        Ok(())
    })
}

unsafe extern "system" fn get_security_by_name(
    fs: *mut FspFileSystem,
    name: PWSTR,
    pattrs: *mut u32,
    sd: *mut c_void,
    psize: *mut usize,
) -> NTSTATUS {
    guarded(fs, "GetSecurityByName", |a| {
        let path = unsafe { path_arg(name) }?;
        let (attr, _) = a.resolve(&path)?;
        if !pattrs.is_null() {
            // SAFETY: valid out pointer.
            unsafe { *pattrs = file_info_for(&attr, a.read_only).file_attributes };
        }
        match a.copy_security(sd, psize) {
            STATUS_SUCCESS => Ok(()),
            other => Err(other),
        }
    })
}

unsafe extern "system" fn open(
    fs: *mut FspFileSystem,
    name: PWSTR,
    _create_options: u32,
    _granted_access: u32,
    pctx: *mut *mut c_void,
    fi: *mut FileInfo,
) -> NTSTATUS {
    guarded(fs, "Open", |a| {
        let path = unsafe { path_arg(name) }?;
        let (attr, parent) = a.resolve(&path)?;
        let handle = OpenHandle {
            ino: attr.ino,
            parent_ino: parent,
            is_dir: attr.kind == InodeKind::Directory,
            path: Mutex::new(path),
            read_fh: Mutex::new(None),
            write_fh: Mutex::new(None),
            dir: Mutex::new(None),
        };
        // SAFETY: valid out pointers.
        unsafe {
            *fi = file_info_for(&attr, a.read_only);
            *pctx = Box::into_raw(Box::new(handle)).cast();
        }
        a.stats.open_handles.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn create(
    fs: *mut FspFileSystem,
    name: PWSTR,
    create_options: u32,
    _granted_access: u32,
    _file_attributes: u32,
    _security: *mut c_void,
    _allocation_size: u64,
    pctx: *mut *mut c_void,
    fi: *mut FileInfo,
) -> NTSTATUS {
    guarded(fs, "Create", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let path = unsafe { path_arg(name) }?;
        let (parent, leaf) = a.resolve_parent(&path)?;
        if names::windows_name_problem(leaf).is_some() {
            return Err(STATUS_OBJECT_NAME_INVALID);
        }
        let is_dir = create_options & win32::FILE_DIRECTORY_FILE != 0;
        let (attr, write_fh) = if is_dir {
            let attr =
                a.rt.block_on(a.vfs.mkdir(parent.ino, leaf, 0o755, 0, 0))
                    .map_err(errno)?;
            (attr, None)
        } else {
            let (attr, fh) =
                a.rt.block_on(a.vfs.create(parent.ino, leaf, 0o644, 0, 0, None))
                    .map_err(errno)?;
            (attr, Some(fh))
        };
        if write_fh.is_some() {
            a.stats.open_write_handles.fetch_add(1, Ordering::Relaxed);
        }
        let handle = OpenHandle {
            ino: attr.ino,
            parent_ino: parent.ino,
            is_dir,
            path: Mutex::new(path),
            read_fh: Mutex::new(None),
            write_fh: Mutex::new(write_fh),
            dir: Mutex::new(None),
        };
        unsafe {
            *fi = file_info_for(&attr, a.read_only);
            *pctx = Box::into_raw(Box::new(handle)).cast();
        }
        a.stats.open_handles.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
}

unsafe extern "system" fn overwrite(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    _file_attributes: u32,
    _replace_attributes: BOOLEAN,
    _allocation_size: u64,
    fi: *mut FileInfo,
) -> NTSTATUS {
    guarded(fs, "Overwrite", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let h = unsafe { handle(ctx) };
        if h.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let fh = a.rt.block_on(a.vfs.open(h.ino, true, true, None)).map_err(errno)?;
        let previous = lock(&h.write_fh).replace(fh);
        match previous {
            Some(old) => {
                let _ = a.rt.block_on(a.vfs.release(old));
            }
            None => {
                a.stats.open_write_handles.fetch_add(1, Ordering::Relaxed);
            }
        }
        unsafe { *fi = a.info(h.ino)? };
        Ok(())
    })
}

unsafe extern "system" fn cleanup(fs: *mut FspFileSystem, ctx: *mut c_void, name: PWSTR, flags: u32) {
    guarded(fs, "Cleanup", |a| {
        let h = unsafe { handle(ctx) };
        if flags & ffi::cleanup::DELETE != 0 {
            if a.read_only {
                return Err(STATUS_MEDIA_WRITE_PROTECTED);
            }
            let path = match unsafe { path_arg(name) } {
                Ok(p) => p,
                Err(_) => lock(&h.path).clone(),
            };
            let (parent, leaf) = a.resolve_parent(&path)?;
            let result = if h.is_dir {
                a.rt.block_on(a.vfs.rmdir(parent.ino, leaf))
            } else {
                a.rt.block_on(a.vfs.unlink(parent.ino, leaf))
            };
            // Cleanup cannot report failure to the caller (a Windows
            // limitation); log it so it shows in diagnostics.
            if let Err(e) = result {
                warn!("delete of {path:?} failed: errno {e}");
                return Err(errno(e));
            }
            return Ok(());
        }
        // The last handle of a modified file closed: start the upload
        // debounce now instead of at final Close.
        if lock(&h.write_fh).is_some() && a.vfs.is_dirty(h.ino) {
            a.vfs.schedule_flush(h.ino);
        }
        Ok(())
    });
}

unsafe extern "system" fn close(fs: *mut FspFileSystem, ctx: *mut c_void) {
    guarded(fs, "Close", |a| {
        // SAFETY: WinFsp calls Close exactly once per successful Open/Create.
        let h = unsafe { Box::from_raw(ctx as *mut OpenHandle) };
        let read_fh = lock(&h.read_fh).take();
        let write_fh = lock(&h.write_fh).take();
        if write_fh.is_some() {
            a.stats.open_write_handles.fetch_sub(1, Ordering::Relaxed);
        }
        for fh in [read_fh, write_fh].into_iter().flatten() {
            if let Err(e) = a.rt.block_on(a.vfs.release(fh)) {
                warn!("release of {:?} failed: errno {e}", lock(&h.path));
            }
        }
        a.stats.open_handles.fetch_sub(1, Ordering::Relaxed);
        Ok(())
    });
}

unsafe extern "system" fn read(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    buf: *mut c_void,
    offset: u64,
    length: u32,
    pbytes: *mut u32,
) -> NTSTATUS {
    guarded(fs, "Read", |a| {
        let h = unsafe { handle(ctx) };
        if h.is_dir {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let size = a.attr(h.ino)?.size;
        if offset >= size {
            return Err(STATUS_END_OF_FILE);
        }
        let fh = a.reader(h)?;
        let want = (length as u64).min(size - offset) as usize;
        // SAFETY: WinFsp guarantees `buf` holds `length` bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), want) };
        let mut done = 0usize;
        // Windows (like FUSE) must not see a short read before EOF.
        while done < want {
            let (data, eof) =
                a.rt.block_on(a.vfs.read(fh, offset + done as u64, (want - done) as u32))
                    .map_err(errno)?;
            let n = data.len().min(want - done);
            if n == 0 {
                break;
            }
            out[done..done + n].copy_from_slice(&data[..n]);
            done += n;
            if eof {
                break;
            }
        }
        if done == 0 {
            return Err(STATUS_END_OF_FILE);
        }
        a.stats.bytes_read.fetch_add(done as u64, Ordering::Relaxed);
        unsafe { *pbytes = done as u32 };
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn write(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    buf: *mut c_void,
    offset: u64,
    length: u32,
    write_to_end_of_file: BOOLEAN,
    constrained_io: BOOLEAN,
    pbytes: *mut u32,
    fi: *mut FileInfo,
) -> NTSTATUS {
    guarded(fs, "Write", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let h = unsafe { handle(ctx) };
        if h.is_dir {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let fh = a.writer(h)?;
        let size = a.attr(h.ino)?.size;
        let offset = if write_to_end_of_file != 0 { size } else { offset };
        let mut length = length as u64;
        if constrained_io != 0 {
            // Paging I/O: never extend the file.
            if offset >= size {
                unsafe {
                    *pbytes = 0;
                    *fi = a.info(h.ino)?;
                }
                return Ok(());
            }
            length = length.min(size - offset);
        }
        // SAFETY: WinFsp guarantees `buf` holds `length` bytes.
        let data = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), length as usize) };
        let mut done = 0usize;
        while done < data.len() {
            let n = a
                .vfs
                .write(h.ino, fh, offset + done as u64, &data[done..])
                .map_err(errno)? as usize;
            if n == 0 {
                return Err(STATUS_DISK_FULL);
            }
            done += n;
        }
        a.stats.bytes_written.fetch_add(done as u64, Ordering::Relaxed);
        unsafe {
            *pbytes = done as u32;
            *fi = a.info(h.ino)?;
        }
        Ok(())
    })
}

unsafe extern "system" fn flush(fs: *mut FspFileSystem, ctx: *mut c_void, fi: *mut FileInfo) -> NTSTATUS {
    guarded(fs, "Flush", |a| {
        if ctx.is_null() {
            // Volume flush: wait for every pending upload.
            if a.read_only {
                return Ok(());
            }
            return match a.wait_committed(None) {
                STATUS_SUCCESS => Ok(()),
                other => Err(other),
            };
        }
        let h = unsafe { handle(ctx) };
        let write_fh = *lock(&h.write_fh);
        if let Some(fh) = write_fh {
            // Surface a failed background upload first, then make
            // FlushFileBuffers mean "committed to the Hub".
            a.rt.block_on(a.vfs.flush(h.ino, fh, None)).map_err(errno)?;
            if a.vfs.is_dirty(h.ino) {
                match a.wait_committed(Some(h.ino)) {
                    STATUS_SUCCESS => {}
                    other => return Err(other),
                }
            }
        }
        if !fi.is_null() {
            unsafe { *fi = a.info(h.ino)? };
        }
        Ok(())
    })
}

unsafe extern "system" fn get_file_info(fs: *mut FspFileSystem, ctx: *mut c_void, fi: *mut FileInfo) -> NTSTATUS {
    guarded(fs, "GetFileInfo", |a| {
        let h = unsafe { handle(ctx) };
        unsafe { *fi = a.info(h.ino)? };
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn set_basic_info(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    _file_attributes: u32,
    _creation_time: u64,
    last_access_time: u64,
    last_write_time: u64,
    _change_time: u64,
    fi: *mut FileInfo,
) -> NTSTATUS {
    guarded(fs, "SetBasicInfo", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let h = unsafe { handle(ctx) };
        // Timestamps are kept for the life of the mount (the Hub stores its
        // own commit time). DOS attributes and creation time have no remote
        // representation and are not persisted.
        let atime = (last_access_time != 0).then(|| time::from_filetime(last_access_time));
        let mtime = (last_write_time != 0).then(|| time::from_filetime(last_write_time));
        if atime.is_some() || mtime.is_some() {
            a.rt.block_on(a.vfs.setattr(h.ino, None, None, None, None, atime, mtime))
                .map_err(errno)?;
        }
        unsafe { *fi = a.info(h.ino)? };
        Ok(())
    })
}

unsafe extern "system" fn set_file_size(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    new_size: u64,
    set_allocation_size: BOOLEAN,
    fi: *mut FileInfo,
) -> NTSTATUS {
    guarded(fs, "SetFileSize", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let h = unsafe { handle(ctx) };
        if h.is_dir {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let size = a.attr(h.ino)?.size;
        // Allocation size only matters when it cuts into the data.
        let target = if set_allocation_size != 0 {
            (new_size < size).then_some(new_size)
        } else {
            (new_size != size).then_some(new_size)
        };
        if let Some(target) = target {
            a.writer(h)?;
            a.rt.block_on(a.vfs.setattr(h.ino, Some(target), None, None, None, None, None))
                .map_err(errno)?;
        }
        unsafe { *fi = a.info(h.ino)? };
        Ok(())
    })
}

unsafe extern "system" fn can_delete(fs: *mut FspFileSystem, ctx: *mut c_void, _name: PWSTR) -> NTSTATUS {
    guarded(fs, "CanDelete", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let h = unsafe { handle(ctx) };
        if h.ino == ROOT_INODE {
            return Err(STATUS_CANNOT_DELETE);
        }
        if h.is_dir {
            let entries = a.rt.block_on(a.vfs.readdir(h.ino)).map_err(errno)?;
            if entries.iter().any(|e| e.name != "." && e.name != "..") {
                return Err(STATUS_DIRECTORY_NOT_EMPTY);
            }
        }
        Ok(())
    })
}

unsafe extern "system" fn rename(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    name: PWSTR,
    new_name: PWSTR,
    replace_if_exists: BOOLEAN,
) -> NTSTATUS {
    guarded(fs, "Rename", |a| {
        if a.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        let h = unsafe { handle(ctx) };
        let old_path = unsafe { path_arg(name) }?;
        let new_path = unsafe { path_arg(new_name) }?;
        let (old_parent, old_leaf) = a.resolve_parent(&old_path)?;
        let (new_parent, new_leaf) = a.resolve_parent(&new_path)?;
        if names::windows_name_problem(new_leaf).is_some() {
            return Err(STATUS_OBJECT_NAME_INVALID);
        }
        match a.rt.block_on(a.vfs.lookup(new_parent.ino, new_leaf)) {
            Ok(target) if target.ino != h.ino => {
                if replace_if_exists == 0 {
                    return Err(STATUS_OBJECT_NAME_COLLISION);
                }
                if target.kind == InodeKind::Directory {
                    return Err(STATUS_ACCESS_DENIED);
                }
            }
            Ok(_) | Err(libc::ENOENT) => {}
            Err(e) => return Err(errno(e)),
        }
        a.rt.block_on(a.vfs.rename(
            old_parent.ino,
            old_leaf,
            new_parent.ino,
            new_leaf,
            replace_if_exists == 0,
        ))
        .map_err(errno)?;
        *lock(&h.path) = new_path;
        Ok(())
    })
}

unsafe extern "system" fn get_security(
    fs: *mut FspFileSystem,
    _ctx: *mut c_void,
    sd: *mut c_void,
    psize: *mut usize,
) -> NTSTATUS {
    guarded(fs, "GetSecurity", |a| match a.copy_security(sd, psize) {
        STATUS_SUCCESS => Ok(()),
        other => Err(other),
    })
}

unsafe extern "system" fn set_security(
    fs: *mut FspFileSystem,
    _ctx: *mut c_void,
    _info: u32,
    _sd: *mut c_void,
) -> NTSTATUS {
    // Access control lists have no representation on the Hub.
    guarded(fs, "SetSecurity", |_| Err(STATUS_NOT_SUPPORTED))
}

impl Adapter {
    fn snapshot(&self, h: &OpenHandle, refresh: bool) -> NtResult<Arc<DirSnapshot<FileInfo>>> {
        let mut slot = lock(&h.dir);
        if !refresh && let Some(snapshot) = slot.as_ref() {
            return Ok(snapshot.clone());
        }
        let entries = self.rt.block_on(self.vfs.readdir(h.ino)).map_err(errno)?;
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.name == "." || entry.name == ".." || entry.kind == InodeKind::Symlink {
                continue;
            }
            if !names::is_representable(&entry.name) {
                self.note_hidden(&entry.name);
                continue;
            }
            // An entry evicted between listing and stat is simply skipped.
            if let Ok(attr) = self.vfs.getattr(entry.ino) {
                items.push((entry.name, file_info_for(&attr, self.read_only)));
            }
        }
        let snapshot = Arc::new(DirSnapshot::new(items));
        *slot = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn add_dir_entry(&self, name: &str, info: FileInfo, buf: *mut c_void, len: u32, pbytes: *mut u32) -> bool {
        let Some(mut entry) = DirInfoBuf::new(name, info) else {
            return true; // unrepresentable length; skip
        };
        let ptr = (&mut *entry as *mut DirInfoBuf).cast::<DirInfo>();
        // SAFETY: entry is a complete DirInfo + name; buf/len/pbytes come from WinFsp.
        unsafe { (self.api.add_dir_info)(ptr, buf, len, pbytes) != 0 }
    }
}

unsafe extern "system" fn read_directory(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    _pattern: PWSTR,
    marker: PWSTR,
    buf: *mut c_void,
    len: u32,
    pbytes: *mut u32,
) -> NTSTATUS {
    guarded(fs, "ReadDirectory", |a| {
        let h = unsafe { handle(ctx) };
        if !h.is_dir {
            return Err(STATUS_NOT_A_DIRECTORY);
        }
        let marker = unsafe { from_wide_ptr(marker, MAX_PATH_UNITS) };
        // A new enumeration (no marker) re-reads the directory; continuation
        // pages reuse the snapshot so entries are neither skipped nor repeated.
        let snapshot = a.snapshot(h, marker.is_none())?;
        let page = snapshot.page_after(marker.as_deref(), h.ino != ROOT_INODE);
        if page.dot && !a.add_dir_entry(".", a.info(h.ino)?, buf, len, pbytes) {
            return Ok(());
        }
        if page.dot_dot && !a.add_dir_entry("..", a.info(h.parent_ino)?, buf, len, pbytes) {
            return Ok(());
        }
        for (name, info) in page.entries {
            if !a.add_dir_entry(name, *info, buf, len, pbytes) {
                return Ok(()); // buffer full; WinFsp asks again with a marker
            }
        }
        // SAFETY: a null DirInfo appends the end-of-directory marker.
        unsafe { (a.api.add_dir_info)(std::ptr::null_mut(), buf, len, pbytes) };
        Ok(())
    })
}

unsafe extern "system" fn get_dir_info_by_name(
    fs: *mut FspFileSystem,
    ctx: *mut c_void,
    name: PWSTR,
    dir_info: *mut DirInfo,
) -> NTSTATUS {
    guarded(fs, "GetDirInfoByName", |a| {
        let h = unsafe { handle(ctx) };
        let name = unsafe { path_arg(name) }?;
        let (attr, actual) = a.lookup_child(h.ino, &name)?;
        let units: Vec<u16> = actual.encode_utf16().collect();
        if units.len() > names::MAX_COMPONENT_UTF16 {
            return Err(STATUS_OBJECT_NAME_NOT_FOUND);
        }
        // SAFETY: WinFsp provides a DirInfo with room for a 255-unit name.
        unsafe {
            let header = &mut *dir_info;
            header.size = (std::mem::size_of::<DirInfo>() + units.len() * 2) as u16;
            header.file_info = file_info_for(&attr, a.read_only);
            header.padding = [0; 24];
            let name_ptr = dir_info.cast::<u8>().add(std::mem::size_of::<DirInfo>()).cast::<u16>();
            std::ptr::copy_nonoverlapping(units.as_ptr(), name_ptr, units.len());
        }
        Ok(())
    })
}

unsafe extern "system" fn dispatcher_stopped(fs: *mut FspFileSystem, normally: BOOLEAN) {
    if normally == 0 {
        // SAFETY: called by WinFsp for our file system.
        let a = unsafe { adapter(fs) };
        a.stopped_abnormally.store(true, Ordering::SeqCst);
        error!("WinFsp stopped serving this volume unexpectedly (driver unloaded or uninstalled?)");
    }
}

fn interface() -> FileSystemInterface {
    FileSystemInterface {
        get_volume_info: Some(get_volume_info),
        get_security_by_name: Some(get_security_by_name),
        create: Some(create),
        open: Some(open),
        overwrite: Some(overwrite),
        cleanup: Some(cleanup),
        close: Some(close),
        read: Some(read),
        write: Some(write),
        flush: Some(flush),
        get_file_info: Some(get_file_info),
        set_basic_info: Some(set_basic_info),
        set_file_size: Some(set_file_size),
        can_delete: Some(can_delete),
        rename: Some(rename),
        get_security: Some(get_security),
        set_security: Some(set_security),
        read_directory: Some(read_directory),
        get_dir_info_by_name: Some(get_dir_info_by_name),
        dispatcher_stopped: Some(dispatcher_stopped),
        ..Default::default()
    }
}

fn volume_params(config: &MountConfig) -> VolumeParams {
    let ttl_ms = config.file_info_timeout.as_millis().min(u32::MAX as u128) as u32;
    let mut flags = vp::CASE_SENSITIVE_SEARCH
        | vp::CASE_PRESERVED_NAMES
        | vp::UNICODE_ON_DISK
        | vp::UM_FILE_CONTEXT_IS_USER_CONTEXT2
        | vp::PASS_QUERY_DIRECTORY_FILE_NAME
        | vp::FLUSH_AND_PURGE_ON_CLEANUP
        | vp::POST_CLEANUP_WHEN_MODIFIED_ONLY;
    if config.read_only {
        flags |= vp::READ_ONLY_VOLUME;
    }
    let mut params = VolumeParams {
        version: std::mem::size_of::<VolumeParams>() as u16,
        sector_size: 512,
        sectors_per_allocation_unit: 8,
        max_component_length: names::MAX_COMPONENT_UTF16 as u16,
        volume_creation_time: time::to_filetime(std::time::SystemTime::now()),
        volume_serial_number: config.volume_serial,
        file_info_timeout: ttl_ms,
        flags,
        flags2: vp::VOLUME_INFO_TIMEOUT_VALID | vp::DIR_INFO_TIMEOUT_VALID | vp::SECURITY_TIMEOUT_VALID,
        volume_info_timeout: 60_000,
        dir_info_timeout: ttl_ms,
        security_timeout: 60_000,
        ..Default::default()
    };
    ffi::copy_wide(&mut params.file_system_name, "hf-mount");
    params
}

/// A mounted WinFsp volume. Dropping it unmounts and frees everything.
pub struct WinFspMount {
    api: &'static WinFspApi,
    fs: *mut FspFileSystem,
    adapter: *mut Adapter,
    _interface: Box<FileSystemInterface>,
    mount_point: String,
    stats: Arc<AdapterStats>,
    stopped_abnormally: Arc<AtomicBool>,
    unmounted: Mutex<bool>,
}

// SAFETY: the raw pointers are only dereferenced by WinFsp's thread-safe
// API and by our callbacks, which only take shared references.
unsafe impl Send for WinFspMount {}
unsafe impl Sync for WinFspMount {}

impl WinFspMount {
    /// Create the volume, assign the drive letter and start serving.
    pub fn mount(vfs: Arc<VirtualFs>, rt: tokio::runtime::Handle, config: MountConfig) -> Result<Self, MountError> {
        let api = super::winfsp::api().map_err(MountError::Driver)?;
        let mount_point = normalize_drive_letter(&config.mount_point)
            .ok_or_else(|| MountError::InvalidMountPoint(config.mount_point.clone()))?;
        let letter = mount_point.chars().next().unwrap_or('?');
        if !super::drives::is_letter_free(letter) {
            return Err(MountError::TargetInUse(mount_point));
        }
        let sid = current_user_sid().map_err(MountError::Security)?;
        let security = SecurityDescriptor::from_sddl(&volume_sddl(&sid)).map_err(MountError::Security)?;

        let params = volume_params(&config);
        let iface = Box::new(interface());
        let device = wide(ffi::DISK_DEVICE_NAME);
        let mut fs: *mut FspFileSystem = std::ptr::null_mut();
        // SAFETY: valid params/interface; `iface` outlives the file system.
        let status = unsafe { (api.file_system_create)(device.as_ptr() as PWSTR, &params, &*iface, &mut fs) };
        if !nt_success(status) {
            return Err(match status {
                STATUS_OBJECT_NAME_NOT_FOUND | STATUS_NO_SUCH_DEVICE | STATUS_OBJECT_PATH_NOT_FOUND => {
                    MountError::DriverUnavailable(status)
                }
                _ => MountError::WinFsp {
                    operation: "FspFileSystemCreate",
                    status,
                },
            });
        }

        let stats = Arc::new(AdapterStats::default());
        let stopped_abnormally = Arc::new(AtomicBool::new(false));
        let adapter = Box::into_raw(Box::new(Adapter {
            api,
            vfs,
            rt,
            read_only: config.read_only,
            case_fallback: config.case_insensitive_fallback,
            flush_wait: config.flush_wait_timeout,
            security,
            label: config.volume_label.encode_utf16().take(32).collect(),
            stats: stats.clone(),
            stopped_abnormally: stopped_abnormally.clone(),
            hidden_logged: Mutex::new(HashSet::new()),
        }));
        // SAFETY: `fs` is a freshly created FSP_FILE_SYSTEM.
        unsafe {
            (*fs.cast::<FspFileSystemPrefix>()).user_context = adapter.cast();
            if config.debug_log {
                (api.set_debug_log)(fs, u32::MAX);
            }
        }

        let cleanup_failed = |fs: *mut FspFileSystem| unsafe {
            (api.file_system_delete)(fs);
            drop(Box::from_raw(adapter));
        };

        let mp = wide(&mount_point);
        // SAFETY: valid file system and NUL-terminated mount point.
        let status = unsafe { (api.set_mount_point)(fs, mp.as_ptr() as PWSTR) };
        if !nt_success(status) {
            cleanup_failed(fs);
            return Err(match status {
                STATUS_OBJECT_NAME_COLLISION | STATUS_ACCESS_DENIED => MountError::TargetInUse(mount_point),
                _ => MountError::WinFsp {
                    operation: "FspFileSystemSetMountPoint",
                    status,
                },
            });
        }
        // SAFETY: valid file system.
        let status = unsafe { (api.start_dispatcher)(fs, config.dispatcher_threads) };
        if !nt_success(status) {
            cleanup_failed(fs);
            return Err(MountError::WinFsp {
                operation: "FspFileSystemStartDispatcher",
                status,
            });
        }
        info!(
            "WinFsp volume mounted at {} ({}, WinFsp {})",
            mount_point,
            if config.read_only { "read-only" } else { "read-write" },
            api.version_string().unwrap_or_else(|| "unknown".to_string())
        );
        Ok(Self {
            api,
            fs,
            adapter,
            _interface: iface,
            mount_point,
            stats,
            stopped_abnormally,
            unmounted: Mutex::new(false),
        })
    }

    pub fn mount_point(&self) -> &str {
        &self.mount_point
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    /// WinFsp stopped the volume on its own (driver unload/uninstall).
    pub fn stopped_abnormally(&self) -> bool {
        self.stopped_abnormally.load(Ordering::SeqCst)
    }

    /// Remove the drive letter and stop serving requests. Open handles fail
    /// from then on. Idempotent.
    pub fn unmount(&self) {
        let mut unmounted = lock(&self.unmounted);
        if *unmounted {
            return;
        }
        // SAFETY: valid file system; both calls are idempotent in WinFsp.
        unsafe {
            (self.api.remove_mount_point)(self.fs);
            (self.api.stop_dispatcher)(self.fs);
        }
        *unmounted = true;
        info!("WinFsp volume {} unmounted", self.mount_point);
    }
}

impl Drop for WinFspMount {
    fn drop(&mut self) {
        self.unmount();
        // SAFETY: the dispatcher is stopped, so no callback can still use the adapter.
        unsafe {
            (self.api.file_system_delete)(self.fs);
            drop(Box::from_raw(self.adapter));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_drive_letter;

    #[test]
    fn drive_letters_normalise() {
        assert_eq!(normalize_drive_letter("h").as_deref(), Some("H:"));
        assert_eq!(normalize_drive_letter("H:").as_deref(), Some("H:"));
        assert_eq!(normalize_drive_letter("x:\\").as_deref(), Some("X:"));
        assert_eq!(normalize_drive_letter(" z: ").as_deref(), Some("Z:"));
        for bad in ["", "1:", "HH:", "C:\\mnt", "\\\\server\\share", "h:x"] {
            assert_eq!(normalize_drive_letter(bad), None, "{bad:?}");
        }
    }
}
