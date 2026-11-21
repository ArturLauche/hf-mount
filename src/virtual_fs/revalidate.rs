//! Apply remote file metadata while preserving local writes, renames and open handles.
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use tracing::{debug, warn};

use super::inode::{InodeEntry, InodeTable};
use super::{InvalKind, VirtualFs};
use crate::hub_api::HeadFileInfo;

/// The state that prompted a HEAD. While the request is in flight the name
/// can move, a local writer can commit, or another remote check can finish.
pub(super) struct FileRevalidation {
    ino: u64,
    path: Arc<str>,
    hash: Option<String>,
    etag: Option<String>,
    size: u64,
    last_revalidated: Option<Instant>,
}

impl FileRevalidation {
    pub(super) fn new(entry: &InodeEntry) -> Self {
        Self {
            ino: entry.inode,
            path: entry.full_path.clone(),
            hash: entry.xet_hash.clone(),
            etag: entry.etag.clone(),
            size: entry.size,
            last_revalidated: entry.last_revalidated,
        }
    }

    pub(super) async fn run(self, vfs: &VirtualFs) {
        let remote = match vfs.hub_client.head_file(&self.path).await {
            Ok(remote) => remote,
            Err(e) => {
                debug!("head_file({}) failed, using cached: {e}", self.path);
                return;
            }
        };
        let mut pending = Vec::new();
        {
            let mut inodes = vfs.inode_table.write().expect("inodes poisoned");
            self.apply(&mut inodes, remote, &mut pending);
        }
        // A synchronous kernel invalidation can wait on a reader that needs
        // the inode lock. Publish the metadata and release the lock first.
        if let Some(invalidate) = vfs.invalidator.get() {
            for (ino, kind) in pending {
                invalidate(ino, kind);
            }
        }
    }

    fn apply(self, inodes: &mut InodeTable, head: Option<HeadFileInfo>, pending: &mut Vec<(u64, InvalKind)>) {
        let Some(entry) = inodes.get_by_path(&self.path) else {
            return;
        };
        if entry.inode != self.ino
            || inodes.is_held_locally(entry)
            || entry.last_revalidated != self.last_revalidated
            || entry.xet_hash != self.hash
            || entry.etag != self.etag
            || entry.size != self.size
        {
            return;
        }
        let Some(head) = head else {
            remove_remote_file(inodes, self.ino, pending);
            return;
        };
        let content_changed = if self.hash.is_some() || head.xet_hash.is_some() {
            self.hash != head.xet_hash
        } else {
            self.etag != head.etag
        };
        if content_changed || head.size.is_some_and(|size| size != self.size) {
            let Some(size) = head.size else {
                warn!("HEAD response missing size for {}, skipping update", self.path);
                return;
            };
            let mtime = head
                .last_modified
                .as_deref()
                .map(crate::hub_api::mtime_from_http_date)
                .unwrap_or_else(SystemTime::now);
            if !inodes.update_remote_file(self.ino, head.xet_hash, head.etag, size, mtime) {
                return;
            }
            queue_file_invalidation(inodes, self.ino, pending);
        } else if let Some(etag) = head.etag {
            // Keep the fallback ETag current even when the Xet hash matches.
            inodes.get_mut(self.ino).expect("validated inode").etag = Some(etag);
        }
        inodes.get_mut(self.ino).expect("validated inode").last_revalidated = Some(Instant::now());
    }
}

/// Queue the kernel invalidation for a file whose remote metadata changed.
/// An inode with open handles may have an in-flight read holding its folio
/// locks; a full page invalidation would block in the kernel waiting on
/// that lock and deadlock against the read (#195), so such inodes get an
/// attribute-only invalidation that never touches pages.
pub(super) fn queue_file_invalidation(
    inode_table: &InodeTable,
    ino: u64,
    inos_to_invalidate: &mut Vec<(u64, InvalKind)>,
) {
    let kind = if inode_table.has_open_handles(ino) {
        InvalKind::AttrOnly
    } else {
        InvalKind::Pages
    };
    inos_to_invalidate.push((ino, kind));
}

/// Drop a remotely deleted file from the inode table and queue the parent
/// directory + inode invalidations. Returns the parent directory, or `None`
/// when the inode is gone, dirty (local writes take precedence until flushed)
/// or being renamed (the deletion is the rename's own commit).
pub(super) fn remove_remote_file(
    inode_table: &mut InodeTable,
    ino: u64,
    inos_to_invalidate: &mut Vec<(u64, InvalKind)>,
) -> Option<u64> {
    let (parent_ino, name) = match inode_table.get(ino) {
        Some(entry) if entry.nlink == 0 || inode_table.is_held_locally(entry) => return None,
        Some(entry) => (entry.parent, entry.name.clone()),
        None => return None,
    };
    inos_to_invalidate.push((parent_ino, InvalKind::Pages));
    // Decide the invalidation kind while the inode is still in the table.
    queue_file_invalidation(inode_table, ino, inos_to_invalidate);
    if inode_table.has_open_handles(ino) {
        // Unlink the pathname but keep the inode as orphan (nlink=0)
        // so open handles can still read/fstat. release() will clean
        // up the orphan. Without this, the file stays visible by name
        // and a recreated file at the same path would collide.
        inode_table.unlink_one(parent_ino, &name);
        debug!("Remote deletion of ino={ino}: unlinked path, kept orphan (open handles)");
    } else {
        inode_table.remove(ino);
    }
    Some(parent_ino)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_fs::inode::{InodeKind, ROOT_INODE};

    #[test]
    fn stale_head_cannot_discard_a_newer_update_a_rename_or_a_dirty_file() {
        let mut table = InodeTable::new(false);
        let ino = table.insert(
            ROOT_INODE,
            "a".into(),
            "a".into(),
            InodeKind::File,
            4,
            SystemTime::UNIX_EPOCH,
            Some("first".into()),
            0o644,
            1000,
            1000,
        );
        let mut pending = Vec::new();
        let before_poll = FileRevalidation::new(table.get(ino).unwrap());
        assert!(table.update_remote_file(ino, Some("second".into()), None, 8, SystemTime::UNIX_EPOCH));
        before_poll.apply(&mut table, None, &mut pending);
        assert_eq!(table.get(ino).unwrap().size, 8);

        let before_move = FileRevalidation::new(table.get(ino).unwrap());
        table.move_child(ino, ROOT_INODE, "a", ROOT_INODE, "b");
        table.update_subtree_paths(ino, "b".into());
        before_move.apply(&mut table, None, &mut pending);
        assert_eq!(table.get_by_path("b").unwrap().inode, ino);

        let before_write = FileRevalidation::new(table.get(ino).unwrap());
        table.get_mut(ino).unwrap().set_dirty();
        before_write.apply(&mut table, None, &mut pending);
        assert!(table.get(ino).unwrap().is_dirty());
        assert!(pending.is_empty());
    }
}
