use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nfsserve::nfs::{
    cookieverf3, fattr3, fileid3, filename3, ftype3, nfs_fh3, nfspath3, nfsstat3, nfsstring, nfstime3, sattr3,
    set_atime, set_gid3, set_mode3, set_mtime, set_size3, set_uid3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener, RpcAuthRequest, RpcAuthorizer};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use tracing::info;

use crate::daemon::DaemonGuard;
use crate::setup::NfsSecurity;
use crate::virtual_fs::inode::{self, InodeKind};
use crate::virtual_fs::{VirtualFs, VirtualFsAttr};

const PORTMAP_PROGRAM: u32 = 100000;

// ── NFS Adapter ────────────────────────────────────────────────────────

pub struct NFSAdapter {
    virtual_fs: Arc<VirtualFs>,
    handle_pool: Arc<Mutex<HandlePool>>,
    read_only: bool,
    security: NfsSecurity,
}

impl NFSAdapter {
    pub fn new(virtual_fs: Arc<VirtualFs>, read_only: bool, security: NfsSecurity) -> Self {
        Self {
            virtual_fs,
            handle_pool: Arc::new(Mutex::new(HandlePool::new())),
            read_only,
            security,
        }
    }

    /// Evict a handle: flush dirty data, then release.
    async fn evict_handle(&self, ino: u64, file_handle: u64) {
        // Flush commits any buffered writes to CAS+Hub before releasing.
        let _ = self.virtual_fs.flush(ino, file_handle, None).await;
        if let Err(e) = self.virtual_fs.release(file_handle).await {
            tracing::error!("NFS evict_handle: release failed for ino={}: errno={}", ino, e);
        }
    }

    /// Get or open a pooled read handle with a shared pin. Cold-read races
    /// converge on a single pooled handle so the per-handle prefetch buffer
    /// absorbs concurrent NFS readahead RPCs instead of spawning duplicate
    /// Xet streams.
    async fn get_or_open_handle(&self, ino: u64) -> Result<u64, nfsstat3> {
        if let Some(handle) = self.acquire_shared(ino) {
            return Ok(handle);
        }
        let file_handle = match self.virtual_fs.open(ino, false, false, None).await {
            Ok(handle) => handle,
            Err(err) => return self.acquire_shared(ino).ok_or_else(|| errno_to_nfs(err)),
        };

        enum Outcome {
            ShareExisting(u64),
            Inserted(InsertResult),
        }
        let outcome = {
            let mut pool = self.handle_pool.lock().expect("handle_pool poisoned");
            if let Some(existing) = pool.acquire_shared(ino) {
                Outcome::ShareExisting(existing)
            } else {
                let result = pool.insert(ino, file_handle);
                pool.acquire_shared(ino);
                Outcome::Inserted(result)
            }
        };
        match outcome {
            Outcome::ShareExisting(existing) => {
                let _ = self.virtual_fs.release(file_handle).await;
                Ok(existing)
            }
            Outcome::Inserted(result) => {
                self.process_insert_result(result).await;
                Ok(file_handle)
            }
        }
    }

    fn acquire_shared(&self, ino: u64) -> Option<u64> {
        self.handle_pool
            .lock()
            .expect("handle_pool poisoned")
            .acquire_shared(ino)
    }

    async fn process_insert_result(&self, result: InsertResult) {
        for (evicted_ino, evicted_handle) in result.evicted {
            self.evict_handle(evicted_ino, evicted_handle).await;
        }
        if let Some(replaced_handle) = result.replaced {
            let _ = self.virtual_fs.release(replaced_handle).await;
        }
    }

    /// Create a file and insert the handle into the pool.
    async fn create_file(
        &self,
        dirid: fileid3,
        filename: &filename3,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let name = nfs_name(filename)?;
        let (attr, file_handle) = self
            .virtual_fs
            .create(dirid, name, mode, uid, gid, None)
            .await
            .map_err(errno_to_nfs)?;
        let ino = attr.ino;
        let fattr = vfs_attr_to_nfs(&attr);
        self.insert_handle(ino, file_handle).await;
        Ok((ino, fattr))
    }

    /// Insert a writable handle into the pool (used after create).
    async fn insert_handle(&self, ino: u64, file_handle: u64) {
        let result = {
            let mut pool = self.handle_pool.lock().expect("handle_pool poisoned");
            pool.insert(ino, file_handle)
        };
        for (evicted_ino, evicted_handle) in result.evicted {
            self.evict_handle(evicted_ino, evicted_handle).await;
        }
        if let Some(replaced_handle) = result.replaced {
            self.evict_handle(ino, replaced_handle).await;
        }
    }
}

#[derive(Clone)]
struct NfsLocalAuthorizer {
    owner_uid: u32,
}

impl NfsLocalAuthorizer {
    fn new(owner_uid: u32) -> Self {
        Self { owner_uid }
    }

    fn is_allowed(&self, request: &RpcAuthRequest) -> bool {
        let Ok(addr) = request.client_addr.parse::<SocketAddr>() else {
            return false;
        };
        if !addr.ip().is_loopback() {
            return false;
        }

        if request.program == PORTMAP_PROGRAM {
            return true;
        }

        if request.auth_flavor != nfsserve::tcp::auth_flavor::AUTH_UNIX {
            return false;
        }

        let uid = request.auth.uid();
        let uid_allowed = uid == 0 || uid == self.owner_uid;
        let reserved_source_port = addr.port() < 1024;
        uid_allowed && reserved_source_port
    }
}

impl RpcAuthorizer for NfsLocalAuthorizer {
    fn authorize(&self, request: &RpcAuthRequest) -> bool {
        self.is_allowed(request)
    }
}

#[async_trait]
impl NFSFileSystem for NFSAdapter {
    fn root_dir(&self) -> fileid3 {
        1
    }

    fn capabilities(&self) -> VFSCapabilities {
        if self.read_only {
            VFSCapabilities::ReadOnly
        } else {
            VFSCapabilities::ReadWrite
        }
    }

    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&self.security.filehandle_secret);
        data.extend_from_slice(&id.to_le_bytes());
        nfs_fh3 { data }
    }

    fn fh_to_id(&self, id: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        if id.data.len() != 24 || id.data[..16] != self.security.filehandle_secret[..] {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        Ok(u64::from_le_bytes(
            id.data[16..24].try_into().map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?,
        ))
    }

    fn serverid(&self) -> cookieverf3 {
        self.security.filehandle_secret[0..8]
            .try_into()
            .expect("fixed-size slice")
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name = nfs_name(filename)?;
        self.virtual_fs
            .lookup(dirid, name)
            .await
            .map(|a| a.ino)
            .map_err(errno_to_nfs)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        self.virtual_fs
            .getattr(id)
            .map(|a| vfs_attr_to_nfs(&a))
            .map_err(errno_to_nfs)
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        // Share the pooled handle across concurrent readers — its prefetch
        // buffer absorbs NFS readahead RPCs efficiently. The shared pin only
        // blocks LRU eviction; concurrent reads still serialize on the
        // per-handle prefetch mutex inside virtual_fs.
        let file_handle = match self.acquire_shared(id) {
            Some(handle) => handle,
            None => self.get_or_open_handle(id).await?,
        };

        let result = self.virtual_fs.read(file_handle, offset, count).await;
        self.handle_pool.lock().expect("handle_pool poisoned").unpin(id);

        match result {
            Ok((bytes, eof)) => Ok((bytes.to_vec(), eof)),
            Err(libc::EBADF) => {
                // unlink/rename can remove() a pinned entry while a read is
                // in flight. Drop the stale entry and retry once with a
                // freshly-opened handle outside the pool.
                {
                    let mut pool = self.handle_pool.lock().expect("handle_pool poisoned");
                    if pool.peek(id) == Some(file_handle) {
                        pool.remove(id);
                    }
                }
                let handle = self
                    .virtual_fs
                    .open(id, false, false, None)
                    .await
                    .map_err(errno_to_nfs)?;
                let result = self.virtual_fs.read(handle, offset, count).await;
                let _ = self.virtual_fs.release(handle).await;
                result.map(|(b, eof)| (b.to_vec(), eof)).map_err(errno_to_nfs)
            }
            Err(err) => Err(errno_to_nfs(err)),
        }
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let entries = self.virtual_fs.readdir(dirid).await.map_err(errno_to_nfs)?;
        let skip = if start_after > 0 {
            entries
                .iter()
                .position(|e| e.ino == start_after)
                .map(|i| i + 1)
                .unwrap_or(0)
        } else {
            0
        };
        let page: Vec<DirEntry> = entries[skip..]
            .iter()
            .take(max_entries)
            .map(|e| {
                let attr = self
                    .virtual_fs
                    .getattr(e.ino)
                    .map(|a| vfs_attr_to_nfs(&a))
                    .unwrap_or_default();
                DirEntry {
                    fileid: e.ino,
                    name: e.name.clone().into_bytes().into(),
                    attr,
                }
            })
            .collect();
        let end = skip + page.len() >= entries.len();
        Ok(ReadDirResult { entries: page, end })
    }

    // ── Write operations ────────────────────────────────────────────────

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        let size = match setattr.size {
            set_size3::size(s) => Some(s),
            set_size3::Void => None,
        };
        let mode = match setattr.mode {
            set_mode3::mode(m) => Some((m & 0o7777) as u16),
            set_mode3::Void => None,
        };
        let uid = match setattr.uid {
            set_uid3::uid(u) => Some(u),
            set_uid3::Void => None,
        };
        let gid = match setattr.gid {
            set_gid3::gid(g) => Some(g),
            set_gid3::Void => None,
        };
        let atime = match setattr.atime {
            set_atime::SET_TO_CLIENT_TIME(t) => Some(nfstime_to_system_time(t)),
            set_atime::SET_TO_SERVER_TIME => Some(SystemTime::now()),
            set_atime::DONT_CHANGE => None,
        };
        let mtime = match setattr.mtime {
            set_mtime::SET_TO_CLIENT_TIME(t) => Some(nfstime_to_system_time(t)),
            set_mtime::SET_TO_SERVER_TIME => Some(SystemTime::now()),
            set_mtime::DONT_CHANGE => None,
        };
        self.virtual_fs
            .setattr(id, size, mode, uid, gid, atime, mtime)
            .await
            .map(|a| vfs_attr_to_nfs(&a))
            .map_err(errno_to_nfs)
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        // Fast path: try the existing pool handle. `virtual_fs.write` is a
        // synchronous `pwrite` with no yield point, so peeking without
        // pinning is safe — the pool can't evict mid-call.
        //
        // The pool may hold a *read-only* handle for this inode from a prior
        // READ RPC. macOS NFS readily issues READs during stat / `ls` to
        // populate its attribute cache, well before any write. Writing to a
        // Lazy/read-only handle returns EBADF, which `errno_to_nfs` maps to
        // `NFS3ERR_STALE`. macOS treats STALE on WRITE as a hard failure and
        // silently drops the pending writes from its buffer — `dd` reports
        // success but the bytes never reach the server. Symptom seen in the
        // wild: `fsync(2)` on the file returns ESTALE.
        //
        // Fix: on EBADF, evict the read-only handle and reopen writable
        // (which materializes the staging file), then retry the write.
        let existing = self.handle_pool.lock().expect("handle_pool poisoned").peek(id);
        if let Some(fh) = existing {
            match self.virtual_fs.write(id, fh, offset, data) {
                Ok(_) => {
                    self.virtual_fs.schedule_flush(id);
                    return self
                        .virtual_fs
                        .getattr(id)
                        .map(|a| vfs_attr_to_nfs(&a))
                        .map_err(errno_to_nfs);
                }
                Err(libc::EBADF) => {
                    // Handle was opened read-only. Evict + remove from pool so
                    // a concurrent caller doesn't peek a freshly-released fh.
                    // Mirrors the analogous EBADF retry in `read()` above.
                    // Guard against removing a different fh: a successful
                    // concurrent upgrader may have already replaced our entry.
                    {
                        let mut pool = self.handle_pool.lock().expect("handle_pool poisoned");
                        if pool.peek(id) == Some(fh) {
                            pool.remove(id);
                        }
                    }
                    self.evict_handle(id, fh).await;
                }
                Err(e) => return Err(errno_to_nfs(e)),
            }
        }

        // Slow path: open a writable handle, run the pwrite, THEN publish the
        // handle to the pool. Two concurrent writers can both reach this
        // branch and open distinct writable handles (open is serialized by
        // VirtualFs's per-inode staging lock, so the calls don't overlap, but
        // they DO produce two distinct fh). If we inserted before the pwrite,
        // the second writer's `insert_handle` would release the first
        // writer's freshly-opened fh as `replaced` — and the first writer's
        // subsequent pwrite would hit EBADF on a closed fh, mapping back to
        // NFS3ERR_STALE (silent data loss, the very symptom this code path
        // exists to avoid). By writing before publishing, the fh stays
        // private to this task until its pwrite completes; nothing else can
        // see it, nothing else can release it.
        let fh = self
            .virtual_fs
            .open(id, true, false, None)
            .await
            .map_err(errno_to_nfs)?;
        if let Err(e) = self.virtual_fs.write(id, fh, offset, data) {
            // Write failed — release the fh we opened so we don't leak it.
            let _ = self.virtual_fs.release(fh).await;
            return Err(errno_to_nfs(e));
        }
        // Publish only on success. Any concurrent writer that reaches this
        // point will release its own fh via the same Err path or replace
        // ours via insert_handle's eviction, but by then our pwrite has
        // committed to the staging file.
        self.insert_handle(id, fh).await;
        // NFS has no close/flush RPC, so schedule a debounced flush after
        // each write to ensure data eventually gets committed to the Hub.
        self.virtual_fs.schedule_flush(id);
        self.virtual_fs
            .getattr(id)
            .map(|a| vfs_attr_to_nfs(&a))
            .map_err(errno_to_nfs)
    }

    async fn create(&self, dirid: fileid3, filename: &filename3, attr: sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        let mode = match attr.mode {
            set_mode3::mode(m) => (m & 0o7777) as u16,
            set_mode3::Void => 0o644,
        };
        let uid = match attr.uid {
            set_uid3::uid(u) => u,
            set_uid3::Void => self.virtual_fs.default_uid(),
        };
        let gid = match attr.gid {
            set_gid3::gid(g) => g,
            set_gid3::Void => self.virtual_fs.default_gid(),
        };
        let (ino, fattr) = self.create_file(dirid, filename, mode, uid, gid).await?;
        // Schedule a flush so empty files (e.g. `touch`) get committed to remote.
        self.virtual_fs.schedule_flush(ino);
        Ok((ino, fattr))
    }

    async fn create_exclusive(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let (ino, _) = self
            .create_file(
                dirid,
                filename,
                0o644,
                self.virtual_fs.default_uid(),
                self.virtual_fs.default_gid(),
            )
            .await?;
        self.virtual_fs.schedule_flush(ino);
        Ok(ino)
    }

    async fn mkdir(&self, dirid: fileid3, dirname: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        let name = nfs_name(dirname)?;
        let attr = self
            .virtual_fs
            .mkdir(
                dirid,
                name,
                0o755,
                self.virtual_fs.default_uid(),
                self.virtual_fs.default_gid(),
            )
            .await
            .map_err(errno_to_nfs)?;
        Ok((attr.ino, vfs_attr_to_nfs(&attr)))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        let name = nfs_name(filename)?;
        let attr = self.virtual_fs.lookup(dirid, name).await.map_err(errno_to_nfs)?;
        let ino = attr.ino;
        match attr.kind {
            InodeKind::Directory => self.virtual_fs.rmdir(dirid, name).await.map_err(errno_to_nfs)?,
            _ => self.virtual_fs.unlink(dirid, name).await.map_err(errno_to_nfs)?,
        }
        // Evict from handle pool so the FD is released immediately.
        // Without this, deleted files' handles linger until LRU-evicted.
        let evicted = self.handle_pool.lock().expect("handle_pool poisoned").remove(ino);
        if let Some(file_handle) = evicted {
            self.evict_handle(ino, file_handle).await;
        }
        Ok(())
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        let from_name = nfs_name(from_filename)?;
        let to_name = nfs_name(to_filename)?;
        // If destination exists and is a different inode from source, it will be
        // unlinked by rename. Evict its stale handle (same reason as remove()).
        // Only resolve source ino when destination exists (the rare overwrite case).
        let dest_ino = self.virtual_fs.lookup(to_dirid, to_name).await.ok().map(|a| a.ino);
        let src_ino = if dest_ino.is_some() {
            self.virtual_fs.lookup(from_dirid, from_name).await.ok().map(|a| a.ino)
        } else {
            None
        };
        self.virtual_fs
            .rename(from_dirid, from_name, to_dirid, to_name, false)
            .await
            .map_err(errno_to_nfs)?;
        // Skip same-inode renames (no-op) to avoid evicting the live handle.
        if let Some(ino) = dest_ino
            && dest_ino != src_ino
        {
            let evicted = self.handle_pool.lock().expect("handle_pool poisoned").remove(ino);
            if let Some(file_handle) = evicted {
                self.evict_handle(ino, file_handle).await;
            }
        }
        Ok(())
    }

    async fn symlink(
        &self,
        dirid: fileid3,
        linkname: &filename3,
        symlink: &nfspath3,
        attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let name = nfs_name(linkname)?;
        let target = std::str::from_utf8(&symlink.0).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        let mode = match attr.mode {
            set_mode3::mode(m) => (m & 0o7777) as u16,
            set_mode3::Void => 0o777,
        };
        let uid = match attr.uid {
            set_uid3::uid(u) => u,
            set_uid3::Void => self.virtual_fs.default_uid(),
        };
        let gid = match attr.gid {
            set_gid3::gid(g) => g,
            set_gid3::Void => self.virtual_fs.default_gid(),
        };
        let vfs_attr = self
            .virtual_fs
            .symlink(dirid, name, target, mode, uid, gid)
            .await
            .map_err(errno_to_nfs)?;
        Ok((vfs_attr.ino, vfs_attr_to_nfs(&vfs_attr)))
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let target = self.virtual_fs.readlink(id).map_err(errno_to_nfs)?;
        Ok(nfsstring(target.into_bytes()))
    }
}

// ── Mount orchestration ────────────────────────────────────────────────

pub async fn mount_nfs(
    virtual_fs: Arc<VirtualFs>,
    mount_point: &Path,
    metadata_ttl_ms: u64,
    read_only: bool,
    security: NfsSecurity,
    daemon_guard: Option<&mut DaemonGuard>,
) -> std::io::Result<()> {
    let params = NfsMountParams {
        metadata_ttl_ms,
        read_only,
        security,
        shutdown: None,
    };
    mount_nfs_with_callback(virtual_fs, mount_point, params, daemon_guard, |_| {}).await
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NfsMountEvent {
    ServerListening { port: u16 },
    MountCommand { command: String },
    Mounted { mount_point: String },
    ShuttingDown { reason: String },
}

/// Cooperative shutdown handle for an NFS mount started with
/// [`mount_nfs_with_callback`]. Cloneable; `request()` may be called from any
/// thread at any point of the mount lifecycle — including while the mount
/// command is still being retried — and makes the mount unmount and return.
#[derive(Clone, Default)]
pub struct MountShutdown {
    inner: Arc<MountShutdownInner>,
}

#[derive(Default)]
struct MountShutdownInner {
    requested: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl MountShutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the mount to stop. Idempotent and thread-safe.
    pub fn request(&self) {
        self.inner.requested.store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_requested(&self) -> bool {
        self.inner.requested.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolve once shutdown has been requested. Race-free against `request`
    /// calls that happen before or while the future is being created.
    async fn wait(&self) {
        loop {
            if self.is_requested() {
                return;
            }
            let mut notified = std::pin::pin!(self.inner.notify.notified());
            notified.as_mut().enable();
            if self.is_requested() {
                return;
            }
            notified.await;
        }
    }
}

/// Resolve when `shutdown` fires; pend forever when no handle was provided.
async fn wait_for_shutdown(shutdown: Option<&MountShutdown>) {
    match shutdown {
        Some(shutdown) => shutdown.wait().await,
        None => std::future::pending().await,
    }
}

/// Mount parameters for [`mount_nfs_with_callback`].
pub struct NfsMountParams {
    pub metadata_ttl_ms: u64,
    pub read_only: bool,
    pub security: NfsSecurity,
    /// Optional cooperative stop handle; see [`MountShutdown`].
    pub shutdown: Option<MountShutdown>,
}

pub async fn mount_nfs_with_callback<F>(
    virtual_fs: Arc<VirtualFs>,
    mount_point: &Path,
    params: NfsMountParams,
    daemon_guard: Option<&mut DaemonGuard>,
    mut on_event: F,
) -> std::io::Result<()>
where
    F: FnMut(NfsMountEvent) + Send,
{
    let NfsMountParams {
        metadata_ttl_ms,
        read_only,
        security,
        shutdown,
    } = params;
    let vfs_for_shutdown = virtual_fs.clone();
    let adapter = NFSAdapter::new(virtual_fs, read_only, security.clone());
    let pool_for_shutdown = adapter.handle_pool.clone();

    // When the poll loop detects a remote change on `ino`, drop the pooled
    // file handle. Pooled handles are bound to the xet_hash captured at
    // open() time (see `open_lazy`); without eviction, subsequent NFS reads
    // keep streaming the pre-update content even though `stat()` already
    // reports the new size — see issue #160.
    let pool = pool_for_shutdown.clone();
    let vfs = vfs_for_shutdown.clone();
    let rt = tokio::runtime::Handle::current();
    vfs_for_shutdown.set_invalidator(Box::new(move |ino| {
        let Some(fh) = pool.lock().expect("handle_pool poisoned").remove(ino) else {
            return;
        };
        let vfs = vfs.clone();
        rt.spawn(async move {
            if let Err(e) = vfs.release(fh).await {
                tracing::debug!("NFS invalidator: release ino={} failed: errno={}", ino, e);
            }
        });
    }));

    let mut listener = NFSTcpListener::bind("127.0.0.1:0", adapter).await?;
    listener.with_export_name(&security.export_name);
    #[cfg(unix)]
    if !security.allow_unsafe_loopback {
        listener.with_authorizer(Arc::new(NfsLocalAuthorizer::new(security.owner_uid)));
    }
    let port = listener.get_listen_port();
    info!("NFS server listening on 127.0.0.1:{}", port);
    on_event(NfsMountEvent::ServerListening { port });

    // Register mount/unmount listener: nfsserve sends `false` on UMNT.
    let (mount_tx, mut mount_rx) = tokio::sync::mpsc::channel::<bool>(1);
    listener.set_mount_listener(mount_tx);

    let mount_point_str = mount_point
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid mount path"))?;
    #[cfg(unix)]
    let nfs_export = format!("127.0.0.1:/{}", security.export_name);

    // Start serving NFS requests *before* the mount command, otherwise
    // mount.nfs has nobody to talk to and fails with EIO.
    let server_handle = tokio::spawn(async move {
        if let Err(e) = listener.handle_forever().await {
            tracing::error!("NFS server error: {}", e);
        }
    });

    // Convert ms to seconds (rounding up so 100ms → 1s, not 0s which disables caching entirely)
    let actimeo = metadata_ttl_ms.div_ceil(1000);

    // A stop request that lands before the mount command runs aborts cleanly
    // instead of mounting and immediately tearing down.
    if shutdown.as_ref().is_some_and(MountShutdown::is_requested) {
        server_handle.abort();
        on_event(NfsMountEvent::ShuttingDown {
            reason: "stop requested".to_string(),
        });
        vfs_for_shutdown.shutdown();
        return Ok(());
    }

    // Platform-specific mount command
    #[cfg(target_os = "macos")]
    {
        // `locallocks` (not `nolocks`): macOS mount_nfs treats `nolocks` as
        // "advisory locking unsupported" and returns ENOTSUP on flock/fcntl,
        // which breaks Python `filelock`, `huggingface_hub`, `datasets`, …
        // `locallocks` keeps lock handling inside the client kernel (no NLM
        // round-trip to the server). `nfsserve` does not implement NLM, so
        // local locking is the only viable option anyway.
        let mut opts = format!("locallocks,vers=3,tcp,rsize=1048576,actimeo={actimeo},port={port},mountport={port}");
        if read_only {
            opts = format!("rdonly,{opts}");
        } else {
            opts = format!("{opts},wsize=1048576");
        }
        let mount_cmd = mount_nfs_command_path();
        on_event(NfsMountEvent::MountCommand {
            command: format!("{} -o {} {} {}", mount_cmd.display(), opts, nfs_export, mount_point_str),
        });
        let mut command = tokio::process::Command::new(mount_cmd);
        command
            .args(["-o", &opts, &nfs_export, mount_point_str])
            .kill_on_drop(true);
        // Race the mount command against a stop request so a hung mount_nfs
        // cannot pin the shutdown; kill_on_drop reaps the child on cancel.
        let status = tokio::select! {
            status = command.status() => status?,
            _ = wait_for_shutdown(shutdown.as_ref()) => {
                server_handle.abort();
                on_event(NfsMountEvent::ShuttingDown {
                    reason: "stop requested".to_string(),
                });
                vfs_for_shutdown.shutdown();
                return Ok(());
            }
        };
        if !status.success() {
            server_handle.abort();
            return Err(std::io::Error::other(format!("mount command failed with {status}")));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut mount_opts = format!("nolock,vers=3,tcp,rsize=1048576,actimeo={actimeo},port={port},mountport={port}");
        if !read_only {
            mount_opts = format!("{mount_opts},wsize=1048576");
        }
        on_event(NfsMountEvent::MountCommand {
            command: format!("mount.nfs -o {mount_opts} {nfs_export} {mount_point_str}"),
        });
        let mut command = if unsafe { libc::getuid() } == 0 {
            let mut command = tokio::process::Command::new("mount.nfs");
            command.args(["-o", &mount_opts, &nfs_export, mount_point_str]);
            command
        } else {
            let mut command = tokio::process::Command::new("sudo");
            command.args(["-n", "mount.nfs", "-o", &mount_opts, &nfs_export, mount_point_str]);
            command
        };
        command.kill_on_drop(true);
        // Race the mount command against a stop request so a hung mount.nfs
        // cannot pin the shutdown; kill_on_drop reaps the child on cancel.
        let output = tokio::select! {
            output = command.output() => output?,
            _ = wait_for_shutdown(shutdown.as_ref()) => {
                server_handle.abort();
                on_event(NfsMountEvent::ShuttingDown {
                    reason: "stop requested".to_string(),
                });
                vfs_for_shutdown.shutdown();
                return Ok(());
            }
        };
        if !output.status.success() {
            server_handle.abort();
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(std::io::Error::other(format!(
                "mount.nfs failed with {}: stdout={stdout} stderr={stderr}",
                output.status
            )));
        }
    }

    #[cfg(windows)]
    let portmapper_handle = match nfsserve::portmap_listener::spawn("127.0.0.1:111".parse().unwrap(), port).await {
        Ok(handle) => handle,
        Err(e) => {
            server_handle.abort();
            return Err(std::io::Error::other(format!(
                "failed to bind portmapper on 127.0.0.1:111: {e} (Administrator required, or another portmap is running)"
            )));
        }
    };
    #[cfg(windows)]
    let skip_auto_mount = std::env::var_os("HF_MOUNT_SKIP_AUTO_MOUNT").is_some();
    #[cfg(not(windows))]
    let skip_auto_mount = false;
    #[cfg(windows)]
    {
        let _ = actimeo; // mount.exe has no actimeo equivalent.
        let opts = String::from("nolock,anon,mtype=hard,rsize=32,wsize=32,timeout=60");
        let share = windows_nfs_share(&security.export_name);
        let mount_target = windows_nfs_mount_target(mount_point_str);
        let mount_cmd = mount_nfs_command_path();
        let cmd = format!("{} -o {opts} {share} {mount_target}", mount_cmd.display());
        on_event(NfsMountEvent::MountCommand { command: cmd.clone() });
        if skip_auto_mount {
            info!(
                "HF_MOUNT_SKIP_AUTO_MOUNT set; server and portmapper are running, mount.exe was not invoked.\n\
                 Run manually in another Administrator shell:\n  {cmd}"
            );
        } else {
            info!("Running: {cmd}");
            // `None` means a stop request cancelled the mount — a clean stop,
            // not a failure.
            let Some(output) =
                mount_windows_nfs_with_retry(&mount_cmd, &opts, &share, &mount_target, shutdown.as_ref()).await?
            else {
                server_handle.abort();
                portmapper_handle.abort();
                on_event(NfsMountEvent::ShuttingDown {
                    reason: "stop requested".to_string(),
                });
                vfs_for_shutdown.shutdown();
                return Ok(());
            };
            if !output.status.success() {
                server_handle.abort();
                portmapper_handle.abort();
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let hint = windows_mount_failure_hint(&output);
                return Err(std::io::Error::other(format!(
                    "mount.exe failed with {}: {hint} cmd=`{cmd}` stdout={} stderr={}",
                    output.status,
                    stdout.trim(),
                    stderr.trim()
                )));
            }
        }
    }

    info!("NFS mount active at {}", mount_point_str);
    on_event(NfsMountEvent::Mounted {
        mount_point: mount_point_str.to_string(),
    });

    // Signal the parent process that the mount is live (daemon mode).
    if let Some(guard) = daemon_guard {
        guard.notify_ready();
    }

    // Wait for unmount signal, server exit, or Ctrl+C.
    // nfsserve sends `true` on MNT and `false` on UMNT — ignore mount events.
    // handle_forever() is an infinite accept() loop that never returns on its own.
    // On Linux, `umount` doesn't always send the UMNT RPC, so we also poll
    // /proc/mounts as a fallback to detect when the mount disappears.
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("Failed to register SIGTERM");
    #[cfg(unix)]
    let sigterm_fut = async move { sigterm.recv().await };
    #[cfg(not(unix))]
    let sigterm_fut = std::future::pending::<Option<()>>();
    let mut server_handle = server_handle;
    tokio::pin!(sigterm_fut);
    let mut server_exited = false;
    // The liveness probe runs as its own select arm: a probe that wedges on a
    // dead mount must not keep the shutdown/signal/UMNT branches from running.
    let mut probe_handle: Option<tokio::task::JoinHandle<bool>> = None;
    // Shutdown-time unmounts are offloaded to a blocking task and awaited
    // (bounded) after the loop, so a wedged umount can't delay the break and
    // the subsequent server/portmapper/VFS teardown.
    let mut unmount_task: Option<tokio::task::JoinHandle<bool>> = None;
    loop {
        tokio::select! {
            msg = mount_rx.recv() => {
                match msg {
                    Some(true) => continue,  // mount event, keep waiting
                    _ => {
                        info!("NFS unmount detected via UMNT, shutting down");
                        on_event(NfsMountEvent::ShuttingDown {
                            reason: "unmount detected".to_string(),
                        });
                        break;
                    }
                }
            }
            _ = &mut server_handle => {
                server_exited = true;
                info!("NFS server exited");
                on_event(NfsMountEvent::ShuttingDown {
                    reason: "server exited".to_string(),
                });
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Received Ctrl+C, unmounting...");
                on_event(NfsMountEvent::ShuttingDown { reason: "Ctrl+C received".to_string() });
                unmount_task = Some(spawn_unmount(mount_point_str));
                break;
            }
            _ = &mut sigterm_fut => {
                info!("Received SIGTERM, unmounting...");
                on_event(NfsMountEvent::ShuttingDown { reason: "termination signal received".to_string() });
                unmount_task = Some(spawn_unmount(mount_point_str));
                break;
            }
            _ = wait_for_shutdown(shutdown.as_ref()) => {
                info!("Shutdown requested, unmounting...");
                on_event(NfsMountEvent::ShuttingDown { reason: "stop requested".to_string() });
                unmount_task = Some(spawn_unmount(mount_point_str));
                break;
            }
            mounted = async { probe_handle.as_mut().expect("probe arm is guarded").await }, if probe_handle.is_some() => {
                probe_handle = None;
                if !skip_auto_mount && !mounted.unwrap_or(false) {
                    info!("NFS mount disappeared, shutting down");
                    on_event(NfsMountEvent::ShuttingDown {
                        reason: "mount disappeared".to_string(),
                    });
                    break;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)), if probe_handle.is_none() => {
                // The probe touches the (possibly wedged) mount, so it runs on
                // the blocking pool and is awaited by the arm above.
                let probe_path = mount_point_str.to_string();
                probe_handle = Some(tokio::task::spawn_blocking(move || is_mounted(&probe_path)));
            }
        }
    }
    // An in-flight probe is abandoned here; its blocking thread finishes (or
    // unwedges) on its own and only returns a bool nobody reads.
    drop(probe_handle);

    // Await the shutdown-time unmount (if any) before tearing the server down,
    // so the server can still service the UMNT RPC — but bound the wait so a
    // wedged umount can't pin teardown indefinitely.
    if let Some(task) = unmount_task {
        match tokio::time::timeout(std::time::Duration::from_secs(10), task).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => tracing::warn!(
                "unmount of {} failed during shutdown; manual cleanup (umount -f) may be required",
                mount_point_str
            ),
            Ok(Err(e)) => tracing::warn!("unmount task for {} failed: {}", mount_point_str, e),
            Err(_) => tracing::warn!(
                "unmount of {} timed out during shutdown; proceeding with teardown",
                mount_point_str
            ),
        }
    }

    // Stop the NFS server task explicitly; dropping the JoinHandle does not
    // cancel a tokio task.
    if !server_exited {
        server_handle.abort();
        let _ = server_handle.await;
    }
    #[cfg(windows)]
    portmapper_handle.abort();

    // Drain handle pool: flush and release all cached handles before VFS shutdown.
    let entries = pool_for_shutdown.lock().expect("handle_pool poisoned").drain();
    for (ino, file_handle) in entries {
        let _ = vfs_for_shutdown.flush(ino, file_handle, None).await;
        let _ = vfs_for_shutdown.release(file_handle).await;
    }

    vfs_for_shutdown.shutdown();
    Ok(())
}

// ── Handle Pool ────────────────────────────────────────────────────────
//
// NFS v3 is stateless — there is no open/close. Every read arrives with
// just a fileid. Our VFS, however, is stateful: open() allocates a file
// handle that tracks prefetch buffers, staging files, etc.
//
// The handle pool bridges the gap: it caches VFS file handles keyed by
// inode, evicting the least-recently-used entry when full. Eviction
// calls flush() then release() — flush commits dirty write data to
// CAS+Hub, release frees the prefetch buffer. A subsequent read on an
// evicted file simply re-opens it (cold open — prefetch restarts).
//
// Each open handle may hold a prefetch buffer (~8 MB worst case), so the
// pool size caps memory usage at roughly capacity × 8 MB.

const HANDLE_POOL_CAPACITY: usize = 64;

struct InsertResult {
    /// LRU evictions of unpinned entries to bring the pool back to capacity.
    evicted: Vec<(u64, u64)>,
    /// Replaced handle for the same ino (e.g. read -> write upgrade).
    replaced: Option<u64>,
}

struct HandleEntry {
    file_handle: u64,
    /// Number of in-flight operations using this handle. Pinned entries
    /// (pin_count > 0) are skipped during LRU eviction so that concurrent
    /// reads never encounter a released handle.
    pin_count: u32,
}

struct HandlePool {
    handles: HashMap<u64, HandleEntry>, // ino -> entry
    order: VecDeque<u64>,               // ino access order (front = oldest)
}

impl HandlePool {
    fn new() -> Self {
        Self {
            handles: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Increment pin_count and return the handle. Does not touch LRU order.
    fn pin(&mut self, ino: u64) -> Option<u64> {
        let entry = self.handles.get_mut(&ino)?;
        entry.pin_count += 1;
        Some(entry.file_handle)
    }

    fn unpin(&mut self, ino: u64) {
        if let Some(entry) = self.handles.get_mut(&ino) {
            entry.pin_count = entry.pin_count.saturating_sub(1);
        }
    }

    /// Pin and promote to MRU. Concurrent readers stack pins on the same
    /// entry — eviction is blocked until every pin is released.
    fn acquire_shared(&mut self, ino: u64) -> Option<u64> {
        let handle = self.pin(ino)?;
        if self.order.back() != Some(&ino) {
            self.order_remove(ino);
            self.order.push_back(ino);
        }
        Some(handle)
    }

    /// Look up a handle without pinning. Safe only when the caller does not
    /// yield before using the handle.
    fn peek(&self, ino: u64) -> Option<u64> {
        self.handles.get(&ino).map(|e| e.file_handle)
    }

    /// Remove an entry from the pool, returning the file handle if present.
    fn remove(&mut self, ino: u64) -> Option<u64> {
        let entry = self.handles.remove(&ino)?;
        self.order_remove(ino);
        Some(entry.file_handle)
    }

    /// Drain all entries, returning (ino, file_handle) pairs.
    fn drain(&mut self) -> Vec<(u64, u64)> {
        self.order.clear();
        self.handles
            .drain()
            .map(|(ino, entry)| (ino, entry.file_handle))
            .collect()
    }

    fn order_remove(&mut self, ino: u64) {
        if let Some(pos) = self.order.iter().position(|&i| i == ino) {
            self.order.remove(pos);
        }
    }

    /// Insert a handle. Returns evicted entries that the caller must release:
    /// - `evicted`: LRU evictions to bring the pool back to capacity
    /// - `replaced`: old handle for the same ino (e.g. replacing read with write)
    ///
    /// Pinned entries (in-flight reads) are skipped during eviction. If all
    /// entries are pinned the pool grows beyond capacity temporarily but
    /// shrinks back on the next insert after pins are released.
    fn insert(&mut self, ino: u64, file_handle: u64) -> InsertResult {
        let replaced = if let Some(old) = self.handles.remove(&ino) {
            self.order_remove(ino);
            Some(old.file_handle)
        } else {
            None
        };
        // Evict the strict LRU (front of order). If it is pinned, grow the
        // pool rather than evicting a newer unpinned entry — otherwise a
        // freshly-inserted create handle could be released before the client
        // sends its first WRITE.
        let mut evicted = Vec::new();
        while self.handles.len() >= HANDLE_POOL_CAPACITY {
            let lru_ino = match self.order.front() {
                Some(&ino) => ino,
                None => break,
            };
            let pinned = self.handles.get(&lru_ino).is_some_and(|entry| entry.pin_count != 0);
            if pinned {
                break;
            }
            self.order.pop_front();
            if let Some(entry) = self.handles.remove(&lru_ino) {
                evicted.push((lru_ino, entry.file_handle));
            }
        }
        self.handles.insert(
            ino,
            HandleEntry {
                file_handle,
                pin_count: 0,
            },
        );
        self.order.push_back(ino);
        InsertResult { evicted, replaced }
    }
}

// ── Conversions ────────────────────────────────────────────────────────

fn nfs_name(filename: &filename3) -> Result<&str, nfsstat3> {
    let name = std::str::from_utf8(&filename.0).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
    inode::validate_child_name(name).map_err(errno_to_nfs)?;
    Ok(name)
}

fn errno_to_nfs(e: i32) -> nfsstat3 {
    match e {
        libc::ENOENT => nfsstat3::NFS3ERR_NOENT,
        libc::EIO => nfsstat3::NFS3ERR_IO,
        libc::EACCES => nfsstat3::NFS3ERR_ACCES,
        libc::EEXIST => nfsstat3::NFS3ERR_EXIST,
        libc::ENOTDIR => nfsstat3::NFS3ERR_NOTDIR,
        libc::EISDIR => nfsstat3::NFS3ERR_ISDIR,
        libc::EINVAL => nfsstat3::NFS3ERR_INVAL,
        libc::EROFS => nfsstat3::NFS3ERR_ROFS,
        libc::ENOTEMPTY => nfsstat3::NFS3ERR_NOTEMPTY,
        libc::EBADF => nfsstat3::NFS3ERR_STALE,
        libc::ENOSPC => nfsstat3::NFS3ERR_NOSPC,
        _ => nfsstat3::NFS3ERR_IO,
    }
}

fn system_time_to_nfstime(t: SystemTime) -> nfstime3 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    nfstime3 {
        seconds: d.as_secs().min(u32::MAX as u64) as u32,
        nseconds: d.subsec_nanos(),
    }
}

/// Run the (blocking) unmount on the blocking pool so a wedged external
/// `umount` can't stall the async shutdown path. Returns a handle the caller
/// awaits with a bounded timeout.
fn spawn_unmount(mount_point: &str) -> tokio::task::JoinHandle<bool> {
    let mount_point = mount_point.to_string();
    tokio::task::spawn_blocking(move || unmount_nfs(&mount_point))
}

/// Unmount `mount_point`, trying the platform syscall first and an external
/// command as fallback. Returns `true` when one of them reported success.
fn unmount_nfs(mount_point: &str) -> bool {
    #[cfg(unix)]
    use std::ffi::CString;

    // Try libc unmount first (no external process dependency).
    #[cfg(unix)]
    if let Ok(c_path) = CString::new(mount_point) {
        #[cfg(target_os = "linux")]
        {
            if unsafe { libc::umount2(c_path.as_ptr(), libc::MNT_DETACH) } == 0 {
                return true;
            }
        }
        #[cfg(target_os = "macos")]
        {
            if unsafe { libc::unmount(c_path.as_ptr(), libc::MNT_FORCE) } == 0 {
                return true;
            }
        }
    }

    // Fallback: external command.
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("/sbin/umount").arg(mount_point).status();
    #[cfg(target_os = "linux")]
    let result = if unsafe { libc::getuid() } == 0 {
        std::process::Command::new("umount").arg(mount_point).status()
    } else {
        std::process::Command::new("sudo")
            .args(["-n", "umount", mount_point])
            .status()
    };
    #[cfg(windows)]
    let result = std::process::Command::new(umount_command_path())
        .args(["-f", &windows_nfs_mount_target(mount_point)])
        .status();

    match result {
        Ok(status) if status.success() => true,
        Ok(status) => {
            tracing::warn!("NFS unmount fallback for {} exited with {}", mount_point, status);
            false
        }
        Err(e) => {
            tracing::warn!("NFS unmount fallback failed for {}: {}", mount_point, e);
            false
        }
    }
}

/// Whether `path` currently has an active mount: checked against the mount
/// table on Linux, the statfs filesystem type on macOS, and a drive/directory
/// probe on Windows. A bare existing directory does not count. Blocking on a
/// wedged mount — keep it off latency-sensitive threads.
pub fn is_mounted(path: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/mounts")
            .map(|s| s.lines().any(|line| line.split_whitespace().nth(1) == Some(path)))
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, check via statfs: a mounted NFS will have f_fstypename = "nfs"
        use std::ffi::CString;
        use std::mem::MaybeUninit;
        let c_path = match CString::new(path) {
            Ok(p) => p,
            Err(_) => return false,
        };
        unsafe {
            let mut buf = MaybeUninit::<libc::statfs>::uninit();
            if libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) == 0 {
                let buf = buf.assume_init();
                let fstype = std::ffi::CStr::from_ptr(buf.f_fstypename.as_ptr());
                fstype.to_bytes() == b"nfs"
            } else {
                false
            }
        }
    }
    #[cfg(windows)]
    {
        // Best-effort for drive-letter mounts. Directory mount points may
        // continue to exist after unmount, but Windows sends UMNT for normal
        // unmounts and Ctrl+C still tears the server down explicitly.
        std::fs::metadata(windows_nfs_probe_path(path)).is_ok()
    }
}

#[cfg(target_os = "macos")]
fn mount_nfs_command_path() -> std::path::PathBuf {
    std::path::PathBuf::from("/sbin/mount_nfs")
}

#[cfg(windows)]
fn mount_nfs_command_path() -> std::path::PathBuf {
    windows_system32_exe("mount.exe")
}

#[cfg(windows)]
fn umount_command_path() -> std::path::PathBuf {
    windows_system32_exe("umount.exe")
}

#[cfg(windows)]
const WINDOWS_ERROR_53_RETRY_ATTEMPTS: usize = 6;
#[cfg(windows)]
const WINDOWS_ERROR_53_RETRY_DELAY_MS: u64 = 300;

/// Run `mount.exe`, retrying transient Network Error 53. Returns `Ok(None)`
/// when a stop request cancelled the mount (a clean stop, not a failure);
/// both the command itself and the backoff sleeps are interruptible.
#[cfg(windows)]
async fn mount_windows_nfs_with_retry(
    mount_cmd: &Path,
    opts: &str,
    share: &str,
    mount_target: &str,
    shutdown: Option<&MountShutdown>,
) -> std::io::Result<Option<std::process::Output>> {
    let mut attempt = 1;
    loop {
        if shutdown.is_some_and(MountShutdown::is_requested) {
            return Ok(None);
        }

        let mut command = tokio::process::Command::new(mount_cmd);
        command.args(["-o", opts, share, mount_target]).kill_on_drop(true);
        let output = tokio::select! {
            output = command.output() => output?,
            _ = wait_for_shutdown(shutdown) => return Ok(None),
        };

        if output.status.success()
            || !windows_mount_output_is_network_error_53(&output)
            || attempt == WINDOWS_ERROR_53_RETRY_ATTEMPTS
        {
            return Ok(Some(output));
        }

        attempt += 1;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(WINDOWS_ERROR_53_RETRY_DELAY_MS)) => {}
            _ = wait_for_shutdown(shutdown) => return Ok(None),
        }
    }
}

#[cfg(windows)]
fn windows_mount_output_is_network_error_53(output: &std::process::Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    windows_mount_text_is_network_error_53(stdout.as_ref()) || windows_mount_text_is_network_error_53(stderr.as_ref())
}

#[cfg(windows)]
fn windows_mount_text_is_network_error_53(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    (text.contains("network error") || text.contains("netzwerkfehler")) && windows_text_contains_code_53(&text)
}

#[cfg(windows)]
fn windows_text_contains_code_53(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_digit()).any(|part| part == "53")
}

#[cfg(windows)]
fn windows_mount_failure_hint(output: &std::process::Output) -> &'static str {
    if windows_mount_output_is_network_error_53(output) {
        "Windows reported Network Error 53 while mounting the local NFS export. Confirm the process is elevated, Client for NFS is enabled, port 111 is available, and localhost NFS/RPC traffic is not blocked."
    } else {
        "Client for NFS may be disabled, the process may not be elevated, or the mount target may be invalid."
    }
}

#[cfg(windows)]
fn windows_nfs_share(export_name: &str) -> String {
    format!(r"\\127.0.0.1\{}", export_name)
}

#[cfg(windows)]
use crate::windows::{drive_letter as windows_drive_letter, system32_exe as windows_system32_exe};

#[cfg(windows)]
fn windows_nfs_mount_target(path: &str) -> String {
    match windows_drive_letter(path) {
        Some(drive) => format!("{drive}:"),
        None => path.to_string(),
    }
}

#[cfg(windows)]
fn windows_nfs_probe_path(path: &str) -> String {
    match windows_drive_letter(path) {
        Some(drive) => format!("{drive}:\\"),
        None => path.to_string(),
    }
}

#[cfg(all(test, windows))]
mod windows_nfs_path_tests {
    use super::*;

    #[test]
    fn drive_letter_mount_targets_are_normalized_for_mount_exe() {
        assert_eq!(windows_nfs_mount_target("Z:"), "Z:");
        assert_eq!(windows_nfs_mount_target("Z:\\"), "Z:");
        assert_eq!(windows_nfs_mount_target("z:/"), "z:");
        assert_eq!(windows_nfs_mount_target(r"C:\hf-mounts\repo"), r"C:\hf-mounts\repo");
    }

    #[test]
    fn drive_letter_probe_paths_use_a_root_separator() {
        assert_eq!(windows_nfs_probe_path("Z:"), "Z:\\");
        assert_eq!(windows_nfs_probe_path("Z:\\"), "Z:\\");
        assert_eq!(windows_nfs_probe_path(r"C:\hf-mounts\repo"), r"C:\hf-mounts\repo");
    }

    #[test]
    fn windows_mount_uses_unc_named_nfs_export_syntax() {
        assert_eq!(windows_nfs_share("hf-mount-test"), r"\\127.0.0.1\hf-mount-test");
    }

    #[test]
    fn detects_english_network_error_53() {
        assert!(windows_mount_text_is_network_error_53(
            "Network Error - 53\nType NET HELPMSG 53 for more information."
        ));
    }

    #[test]
    fn detects_german_network_error_53() {
        assert!(windows_mount_text_is_network_error_53(
            "Netzwerkfehler - 53\nGeben Sie \"NET HELPMSG 53\" ein, um weitere Informationen zu erhalten."
        ));
    }

    #[test]
    fn ignores_other_mount_errors() {
        assert!(!windows_mount_text_is_network_error_53("Network Error - 67"));
        assert!(!windows_mount_text_is_network_error_53("Network Error - 153"));
        assert!(!windows_mount_text_is_network_error_53(
            "The network path was not found."
        ));
    }
}

fn vfs_attr_to_nfs(attr: &VirtualFsAttr) -> fattr3 {
    let ftype = match attr.kind {
        InodeKind::File => ftype3::NF3REG,
        InodeKind::Directory => ftype3::NF3DIR,
        InodeKind::Symlink => ftype3::NF3LNK,
    };
    fattr3 {
        ftype,
        mode: attr.perm as u32,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        size: attr.size,
        used: attr.blocks * 512,
        rdev: specdata3 {
            specdata1: 0,
            specdata2: 0,
        },
        fsid: 0,
        fileid: attr.ino,
        atime: system_time_to_nfstime(attr.atime),
        mtime: system_time_to_nfstime(attr.mtime),
        ctime: system_time_to_nfstime(attr.ctime),
    }
}

fn nfstime_to_system_time(t: nfstime3) -> SystemTime {
    let nsec = t.nseconds.min(999_999_999);
    UNIX_EPOCH + std::time::Duration::new(t.seconds as u64, nsec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_mocks::{MockHub, MockXet, TestOpts, make_test_vfs};

    fn test_nfs_security() -> NfsSecurity {
        NfsSecurity {
            owner_uid: 1000,
            allow_unsafe_loopback: false,
            export_name: "hf-mount-test".to_string(),
            filehandle_secret: [7; 16],
        }
    }

    #[test]
    fn nfs_name_rejects_path_components() {
        for bytes in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b"] {
            let name = nfsstring(bytes.to_vec());
            assert_eq!(nfs_name(&name), Err(nfsstat3::NFS3ERR_INVAL));
        }
        let ok = nfsstring(b"model.bin".to_vec());
        assert_eq!(nfs_name(&ok), Ok("model.bin"));
    }

    #[test]
    fn nfs_authorizer_requires_loopback_authsys_owner_and_reserved_port() {
        let auth = NfsLocalAuthorizer::new(1000);
        let mut request = RpcAuthRequest {
            client_addr: "127.0.0.1:900".to_string(),
            auth_flavor: nfsserve::tcp::auth_flavor::AUTH_UNIX,
            auth: nfsserve::tcp::auth_unix::new(1000, 1000, vec![]),
            program: nfsserve::nfs::PROGRAM,
            procedure: 1,
        };
        assert!(auth.authorize(&request));

        request.client_addr = "127.0.0.1:9000".to_string();
        assert!(!auth.authorize(&request));

        request.client_addr = "192.0.2.1:900".to_string();
        assert!(!auth.authorize(&request));

        request.client_addr = "127.0.0.1:900".to_string();
        request.auth = nfsserve::tcp::auth_unix::new(2000, 2000, vec![]);
        assert!(!auth.authorize(&request));

        request.auth = nfsserve::tcp::auth_unix::new(0, 0, vec![]);
        assert!(auth.authorize(&request));

        request.auth_flavor = nfsserve::tcp::auth_flavor::AUTH_NULL;
        assert!(!auth.authorize(&request));
    }

    #[test]
    fn nfs_filehandles_include_secret_material() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let vfs = make_test_vfs(MockHub::new(), MockXet::new(), TestOpts::default(), &rt);
        let adapter = NFSAdapter::new(vfs, false, test_nfs_security());

        let fh = adapter.id_to_fh(42);
        assert_eq!(adapter.fh_to_id(&fh), Ok(42));

        let mut forged = fh.clone();
        forged.data[0] ^= 1;
        assert_eq!(adapter.fh_to_id(&forged), Err(nfsstat3::NFS3ERR_BADHANDLE));
    }

    /// Regression: a WRITE that arrives after a prior READ has populated the
    /// pool with a Lazy/read-only handle must NOT surface as `NFS3ERR_STALE`.
    /// Pre-fix, `nfs.rs::write()` peeked the read-only handle, called
    /// `virtual_fs.write()` which returned EBADF, and `errno_to_nfs` mapped
    /// that to STALE — at which point macOS NFS silently discards the write.
    ///
    /// The fix: on EBADF, evict the read-only handle and re-open writable,
    /// then retry. This test exercises that exact sequence.
    #[test]
    fn write_after_read_upgrades_handle_instead_of_returning_stale() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let hub = MockHub::new();
        hub.add_file("file.txt", 11, Some("orig_hash"), None);
        let xet = MockXet::new();
        xet.add_file("orig_hash", b"hello world");

        let vfs = make_test_vfs(
            hub.clone(),
            xet.clone(),
            TestOpts {
                advanced_writes: true,
                ..Default::default()
            },
            &rt,
        );

        // NFS adapter under test (read-write, like a real bucket mount).
        let adapter = NFSAdapter::new(vfs.clone(), false, test_nfs_security());

        rt.block_on(async {
            // Resolve the ino so we don't hard-code it.
            let name = nfsstring(b"file.txt".to_vec());
            let ino = adapter.lookup(1, &name).await.expect("lookup");

            // Step 1: a READ populates the pool with a read-only (Lazy) handle.
            let (_buf, _eof) = adapter.read(ino, 0, 11).await.expect("read");
            let pooled_fh_after_read = adapter
                .handle_pool
                .lock()
                .expect("poisoned")
                .peek(ino)
                .expect("pool entry");

            // Step 2: the critical operation — WRITE on the same inode.
            // Pre-fix: this returned Err(NFS3ERR_STALE). Post-fix: it must
            // upgrade the handle and succeed.
            let attr = adapter
                .write(ino, 6, b"RUST!")
                .await
                .expect("write must not return STALE");

            // The new attributes reflect the write.
            assert_eq!(attr.size, 11, "file size should be unchanged (in-place edit)");

            // The pool's handle must have been swapped for a writable one
            // (the slow path inserts a fresh handle after the upgrade).
            let pooled_fh_after_write = adapter
                .handle_pool
                .lock()
                .expect("poisoned")
                .peek(ino)
                .expect("pool entry");
            assert_ne!(
                pooled_fh_after_read, pooled_fh_after_write,
                "pool handle must be different after the upgrade (old Lazy → new writable)"
            );
        });
    }

    /// A second WRITE on the same ino reuses the now-writable pool handle
    /// (fast path: no further open/release dance).
    #[test]
    fn second_write_reuses_writable_handle() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let hub = MockHub::new();
        hub.add_file("file.txt", 11, Some("orig_hash"), None);
        let xet = MockXet::new();
        xet.add_file("orig_hash", b"hello world");

        let vfs = make_test_vfs(
            hub.clone(),
            xet.clone(),
            TestOpts {
                advanced_writes: true,
                ..Default::default()
            },
            &rt,
        );
        let adapter = NFSAdapter::new(vfs.clone(), false, test_nfs_security());

        rt.block_on(async {
            let name = nfsstring(b"file.txt".to_vec());
            let ino = adapter.lookup(1, &name).await.expect("lookup");

            // First read + write triggers the upgrade.
            adapter.read(ino, 0, 11).await.expect("read");
            adapter.write(ino, 0, b"A").await.expect("first write");
            let fh_after_first_write = adapter
                .handle_pool
                .lock()
                .expect("poisoned")
                .peek(ino)
                .expect("pool entry");

            // Second write should take the fast path (no new open).
            adapter.write(ino, 1, b"B").await.expect("second write");
            let fh_after_second_write = adapter
                .handle_pool
                .lock()
                .expect("poisoned")
                .peek(ino)
                .expect("pool entry");

            assert_eq!(
                fh_after_first_write, fh_after_second_write,
                "fast path must reuse the writable handle"
            );
        });
    }

    /// A WRITE on a file that was never opened goes through the slow path
    /// directly (no fast-path EBADF). Verifies the slow path stands alone.
    #[test]
    fn write_without_prior_read_opens_writable_directly() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let hub = MockHub::new();
        hub.add_file("file.txt", 11, Some("orig_hash"), None);
        let xet = MockXet::new();
        xet.add_file("orig_hash", b"hello world");

        let vfs = make_test_vfs(
            hub.clone(),
            xet.clone(),
            TestOpts {
                advanced_writes: true,
                ..Default::default()
            },
            &rt,
        );
        let adapter = NFSAdapter::new(vfs.clone(), false, test_nfs_security());

        rt.block_on(async {
            let name = nfsstring(b"file.txt".to_vec());
            let ino = adapter.lookup(1, &name).await.expect("lookup");

            // Pool is empty for this ino; write goes straight to slow path.
            assert!(adapter.handle_pool.lock().unwrap().peek(ino).is_none());
            adapter.write(ino, 0, b"X").await.expect("write");
            assert!(
                adapter.handle_pool.lock().unwrap().peek(ino).is_some(),
                "slow path must have inserted a writable handle"
            );
        });
    }

    #[test]
    fn handle_pool_basic() {
        let mut pool = HandlePool::new();
        assert!(pool.peek(1).is_none());

        let result = pool.insert(1, 100);
        assert!(result.evicted.is_empty());
        assert!(result.replaced.is_none());
        assert_eq!(pool.peek(1), Some(100));
    }

    #[test]
    fn handle_pool_lru_eviction() {
        let mut pool = HandlePool::new();
        for i in 0..HANDLE_POOL_CAPACITY as u64 {
            pool.insert(i, i + 1000);
        }
        // Promote ino=0 to MRU so the LRU is now ino=1.
        pool.acquire_shared(0);
        pool.unpin(0);

        let result = pool.insert(999, 9999);
        assert_eq!(result.evicted.len(), 1);
        assert_eq!(result.evicted[0], (1, 1001));
        assert!(result.replaced.is_none());

        assert!(pool.peek(1).is_none());
        assert_eq!(pool.peek(999), Some(9999));
    }

    #[test]
    fn handle_pool_no_duplicate_on_reinsert() {
        let mut pool = HandlePool::new();
        pool.insert(1, 100);
        pool.insert(2, 200);
        // Re-insert ino=1 with new handle (e.g. replacing read with write)
        let result = pool.insert(1, 101);
        assert_eq!(result.replaced, Some(100), "old handle should be returned for release");
        assert!(result.evicted.is_empty());

        assert_eq!(pool.peek(1), Some(101));
        // order should have exactly 2 entries, not 3
        assert_eq!(pool.order.len(), 2);
    }

    #[test]
    fn handle_pool_remove() {
        let mut pool = HandlePool::new();
        pool.insert(1, 100);
        pool.insert(2, 200);
        pool.insert(3, 300);

        assert_eq!(pool.remove(2), Some(200));
        assert!(pool.peek(2).is_none());
        assert_eq!(pool.order.len(), 2);
        assert_eq!(pool.handles.len(), 2);
        // Remaining entries still work
        assert_eq!(pool.peek(1), Some(100));
        assert_eq!(pool.peek(3), Some(300));
    }

    #[test]
    fn handle_pool_drain() {
        let mut pool = HandlePool::new();
        pool.insert(1, 100);
        pool.insert(2, 200);
        pool.insert(3, 300);

        let entries = pool.drain();
        assert_eq!(entries.len(), 3);
        assert!(pool.handles.is_empty());
        assert!(pool.order.is_empty());
        // Entries contain all inserted pairs
        assert!(entries.contains(&(1, 100)));
        assert!(entries.contains(&(2, 200)));
        assert!(entries.contains(&(3, 300)));
    }

    #[test]
    fn handle_pool_remove_nonexistent() {
        let mut pool = HandlePool::new();
        pool.insert(1, 100);
        assert_eq!(pool.remove(999), None); // no-op
        assert_eq!(pool.peek(1), Some(100));
        assert_eq!(pool.order.len(), 1);
    }

    #[test]
    fn handle_pool_pinned_lru_grows_pool() {
        let mut pool = HandlePool::new();
        // Fill pool to capacity. ino=0 is the LRU entry.
        for i in 0..HANDLE_POOL_CAPACITY as u64 {
            pool.insert(i, i + 1000);
        }
        // Pin ino=0 in place so the LRU is unevictable.
        pool.pin(0);
        assert_eq!(pool.order.front(), Some(&0));

        // Insert one more — the pool grows rather than evicting a newer
        // unpinned entry (which could be a freshly-created file's handle).
        let result = pool.insert(999, 9999);
        assert!(result.evicted.is_empty(), "pinned LRU should not trigger eviction");
        assert_eq!(pool.handles.len(), HANDLE_POOL_CAPACITY + 1);

        // All originals still present.
        for i in 0..HANDLE_POOL_CAPACITY as u64 {
            assert!(pool.handles.contains_key(&i));
        }
        pool.unpin(0);
    }

    #[test]
    fn handle_pool_unpin_allows_eviction() {
        let mut pool = HandlePool::new();
        for i in 0..HANDLE_POOL_CAPACITY as u64 {
            pool.insert(i, i + 1000);
        }
        // Pin in place (no promotion), then unpin so ino=0 is evictable LRU.
        pool.pin(0);
        pool.unpin(0);
        assert_eq!(pool.order.front(), Some(&0));

        // ino=0 is unpinned and at the front of the order, so it evicts.
        let result = pool.insert(999, 9999);
        assert_eq!(result.evicted.len(), 1);
        assert_eq!(result.evicted[0].0, 0, "unpinned LRU should evict normally");
    }

    #[test]
    fn handle_pool_all_pinned_grows_beyond_capacity() {
        let mut pool = HandlePool::new();
        for i in 0..HANDLE_POOL_CAPACITY as u64 {
            pool.insert(i, i + 1000);
            pool.pin(i);
        }
        // All entries pinned — insert should succeed with no eviction
        let result = pool.insert(999, 9999);
        assert!(result.evicted.is_empty(), "no eviction when all entries are pinned");
        assert_eq!(pool.handles.len(), HANDLE_POOL_CAPACITY + 1);

        // Unpin all
        for i in 0..HANDLE_POOL_CAPACITY as u64 {
            pool.unpin(i);
        }

        // Next insert should reclaim overflow entries
        let result = pool.insert(998, 9998);
        // Should evict enough to bring pool back to capacity (evict 2: the
        // overflow entry 999 is MRU, so oldest unpinned entries are evicted)
        assert!(result.evicted.len() >= 2, "pool should shrink after overflow");
        assert!(pool.handles.len() <= HANDLE_POOL_CAPACITY + 1);
    }

    #[test]
    fn handle_pool_acquire_shared_stacks_pins() {
        let mut pool = HandlePool::new();
        assert!(pool.acquire_shared(1).is_none());

        pool.insert(1, 100);
        assert_eq!(pool.acquire_shared(1), Some(100));
        assert_eq!(pool.acquire_shared(1), Some(100));
        assert_eq!(pool.handles[&1].pin_count, 2);

        pool.unpin(1);
        pool.unpin(1);
        assert_eq!(pool.handles[&1].pin_count, 0);
    }
}
