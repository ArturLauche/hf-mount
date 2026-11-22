//! Overlay path mutations span the backing filesystem and the in-memory
//! namespace. Serialize those mutations without blocking inode-only reads.

use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLockWriteGuard, Weak};
use std::time::{Instant, SystemTime};

use tokio::sync::{OwnedRwLockWriteGuard, RwLock as AsyncRwLock, RwLockReadGuard};

use super::{InodeKind, InodeTable, LocalHandleKind, OpenFile, VirtualFs, VirtualFsResult, inode};
use crate::overlay::{OverlayBacking, OverlayDirEntry};

pub(super) struct OverlaySnapshot {
    pub entries: Vec<OverlayDirEntry>,
    pub generation: u64,
}

pub(super) struct MountedOverlay {
    pub backing: Arc<OverlayBacking>,
    gate: Arc<AsyncRwLock<()>>,
    // Existing descriptors can write during a directory scan. Use the dirty
    // generation on each overlay inode to recognize metadata newer than it.
    generation: AtomicU64,
    // Public VFS operations borrow &self. A blocking namespace transaction
    // must own the VFS until it publishes its result, even if its caller exits.
    owner: Weak<VirtualFs>,
    #[cfg(test)]
    pub(super) before_io: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl MountedOverlay {
    pub fn new(backing: OverlayBacking, owner: Weak<VirtualFs>) -> Self {
        Self {
            backing: Arc::new(backing),
            gate: Arc::default(),
            generation: AtomicU64::new(0),
            owner,
            #[cfg(test)]
            before_io: Default::default(),
        }
    }

    /// Read the backing on a blocking worker. The caller holds a shared gate
    /// until it merges the listing, so a rename cannot make the snapshot stale.
    pub async fn entries(
        &self,
        directory: String,
        only_if_exists: Option<String>,
    ) -> VirtualFsResult<Option<OverlaySnapshot>> {
        let generation = self.generation.load(Ordering::Relaxed);
        let backing = self.backing.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(path) = only_if_exists
                && !backing.exists(&path).map_err(|e| crate::error::io_errno(&e))?
            {
                return Ok(None);
            }
            // A remote directory need not have a local backing directory.
            match backing.read_dir(&directory) {
                Ok(entries) => Ok(Some(OverlaySnapshot { entries, generation })),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(crate::error::io_errno(&err)),
            }
        })
        .await
        .map_err(|_| libc::EIO)?
    }
}

pub(super) struct CreatedEntry {
    pub ino: u64,
    pub path: String,
    pub dirty_generation: u64,
    pub overlay_file: Option<File>,
}

pub(super) fn allow_missing(result: std::io::Result<()>) -> VirtualFsResult<()> {
    match result {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result.map_err(|e| crate::error::io_errno(&e)),
    }
}

impl VirtualFs {
    pub(super) fn overlay_backing(&self) -> Option<&OverlayBacking> {
        self.overlay_mount.as_ref().map(|overlay| overlay.backing.as_ref())
    }

    pub(super) fn mark_local_dirty(&self, entry: &mut inode::InodeEntry) {
        if let Some(overlay) = &self.overlay_mount {
            // All writers already hold the inode table's write lock. Scans
            // only need an atomic read before they start filesystem I/O.
            let generation = overlay.generation.load(Ordering::Relaxed).saturating_add(1);
            overlay.generation.store(generation, Ordering::Relaxed);
            entry.dirty_generation = generation;
        } else {
            entry.set_dirty();
        }
    }

    /// An unlinked overlay file can only be addressed by a surviving
    /// descriptor. Its former name may already belong to a replacement.
    pub(super) fn unlinked_overlay_handle(&self, ino: u64, needs_write: bool) -> VirtualFsResult<Option<Arc<File>>> {
        if !self.overlay() {
            return Ok(None);
        }
        let table = self.inode_table.read().expect("inodes poisoned");
        let entry = table.get(ino).ok_or(libc::ENOENT)?;
        if entry.nlink != 0 {
            return Ok(None);
        }
        if !entry.is_dirty() {
            return Err(libc::EPERM);
        }
        self.open_files
            .read()
            .expect("open_files poisoned")
            .values()
            .find_map(|open| match open {
                OpenFile::Local { ino: owner, file, kind }
                    if *owner == ino
                        && *kind != LocalHandleKind::CachedRead
                        && (!needs_write || *kind == LocalHandleKind::BackingWrite) =>
                {
                    Some(file.clone())
                }
                _ => None,
            })
            .map(Some)
            .ok_or(libc::EBADF)
    }

    pub(super) async fn overlay_read_guard(&self) -> Option<RwLockReadGuard<'_, ()>> {
        match &self.overlay_mount {
            Some(overlay) => Some(overlay.gate.read().await),
            None => None,
        }
    }

    /// Once started, both halves of an overlay change finish on the same
    /// worker. Return the gate with the result so create can install its open
    /// handle before the name is reused. A cancelled create leaks no handle.
    pub(super) async fn change_namespace<T: Send + 'static>(
        &self,
        change: impl FnOnce(&Self) -> VirtualFsResult<T> + Send + 'static,
    ) -> VirtualFsResult<(T, Option<OwnedRwLockWriteGuard<()>>)> {
        let Some(overlay) = &self.overlay_mount else {
            return Ok((change(self)?, None));
        };
        let gate = overlay.gate.clone().write_owned().await;
        let owner = overlay.owner.upgrade().ok_or(libc::EIO)?;
        tokio::task::spawn_blocking(move || Ok((change(&owner)?, Some(gate))))
            .await
            .map_err(|_| libc::EIO)?
    }

    /// Called under the overlay write gate. Pin the directories against LRU
    /// eviction between validation and publication. Never hold the inode table
    /// while the backing filesystem can block.
    pub(super) fn overlay_io<'a, T>(
        &'a self,
        table: RwLockWriteGuard<'a, InodeTable>,
        parents: &[u64],
        io: impl FnOnce() -> VirtualFsResult<T>,
    ) -> VirtualFsResult<(RwLockWriteGuard<'a, InodeTable>, T)> {
        for &ino in parents {
            table.bump_open_handles(ino);
        }
        drop(table);
        #[cfg(test)]
        if let Some(pause) = self
            .overlay_mount
            .as_ref()
            .expect("mounted overlay")
            .before_io
            .lock()
            .unwrap()
            .take()
        {
            pause();
        }
        let result = io();
        let table = self.inode_table.write().expect("inodes poisoned");
        for &ino in parents {
            table.drop_open_handles(ino);
        }
        Ok((table, result?))
    }

    pub(super) fn create_entry(
        &self,
        parent: u64,
        name: String,
        kind: InodeKind,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> VirtualFsResult<CreatedEntry> {
        let mut inodes = self.inode_table.write().expect("inodes poisoned");
        let parent_entry = inodes.get(parent).ok_or(libc::ENOENT)?;
        if parent_entry.kind != InodeKind::Directory {
            return Err(libc::ENOTDIR);
        }
        if inodes.lookup_child(parent, &name).is_some() {
            return Err(libc::EEXIST);
        }
        let path = inode::child_path(&parent_entry.full_path, &name);
        let is_dir = kind == InodeKind::Directory;
        let mut overlay_file = None;
        if let Some(overlay) = &self.overlay_mount {
            (inodes, overlay_file) = self.overlay_io(inodes, &[parent], || {
                let result = if is_dir {
                    overlay
                        .backing
                        .create_parent_dirs(&path)
                        .and_then(|()| overlay.backing.create_dir(&path, mode))
                        .map(|()| None)
                } else {
                    overlay.backing.create_file(&path, mode).map(Some)
                };
                result.map_err(|e| crate::error::io_errno(&e))
            })?;
        }
        let now = SystemTime::now();
        let ino = inodes.insert(parent, name, path.clone(), kind, 0, now, None, mode, uid, gid);
        let entry = inodes.get_mut(ino).expect("new inode");
        if is_dir {
            entry.children_loaded_at = Some(Instant::now());
            entry.created_locally = true;
        }
        if !is_dir || self.overlay() {
            self.mark_local_dirty(entry);
        }
        let dirty_generation = entry.dirty_generation;
        inodes.touch_parent(parent, now);
        if !is_dir {
            inodes.touch(ino);
        }
        drop(inodes);
        self.negative_cache_remove(&path);
        if !is_dir && let Some(fm) = &self.flush_manager {
            fm.cancel_delete(&path);
        }
        Ok(CreatedEntry {
            ino,
            path,
            dirty_generation,
            overlay_file,
        })
    }
}
