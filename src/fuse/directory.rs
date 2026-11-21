//! A FUSE offset is a cookie within one open directory stream. Keep its
//! listing stable until rewind so changes between two replies cannot shift
//! the meaning of a cookie. Only handles that actually enumerate need a list.

use std::sync::{Arc, Mutex};

use fuser::FileHandle;

use super::FuseAdapter;
use crate::virtual_fs::{DirectoryEntry, VirtualFs, VirtualFsResult};

pub(super) struct OpenDirectory {
    ino: u64,
    vfs: Arc<VirtualFs>,
    entries: Mutex<Option<Vec<DirectoryEntry<Arc<str>>>>>,
}

impl Drop for OpenDirectory {
    fn drop(&mut self) {
        self.vfs.drop_open_handles(self.ino);
    }
}

impl FuseAdapter {
    pub(super) fn open_directory(&self, ino: u64) -> VirtualFsResult<FileHandle> {
        self.virtual_fs.pin_directory(ino)?;
        let dir = Arc::new(OpenDirectory {
            ino,
            vfs: self.virtual_fs.clone(),
            entries: Mutex::new(None),
        });
        let fh = FileHandle(self.virtual_fs.alloc_file_handle());
        self.directories.lock().expect("directories poisoned").insert(fh, dir);
        Ok(fh)
    }

    pub(super) fn read_directory(
        &self,
        fh: FileHandle,
        offset: u64,
        mut add: impl FnMut(u64, &DirectoryEntry<Arc<str>>) -> bool,
    ) -> VirtualFsResult<()> {
        let dir = self
            .directories
            .lock()
            .expect("directories poisoned")
            .get(&fh)
            .cloned()
            .ok_or(libc::EBADF)?;
        // FUSE callbacks run on blocking threads. Only readers of this handle
        // serialize here; neither Hub I/O nor filling a reply holds the registry lock.
        let mut slot = dir.entries.lock().expect("directory listing poisoned");
        if offset == 0 {
            *slot = None;
        }
        let entries = match &*slot {
            Some(entries) => entries,
            None => slot.insert(
                self.runtime
                    .block_on(dir.vfs.collect_dir_entries(dir.ino, Arc::clone))?,
            ),
        };
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        for (index, entry) in entries.iter().enumerate().skip(start) {
            if add(index as u64 + 1, entry) {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn close_directory(&self, fh: FileHandle) -> VirtualFsResult<()> {
        let dir = self.directories.lock().expect("directories poisoned").remove(&fh);
        // Drop (potentially thousands of names) outside the registry lock.
        dir.map(|_| ()).ok_or(libc::EBADF)
    }
}

#[cfg(test)]
mod tests;
