//! Windows filesystem semantics for the WinFsp backend that do not depend on
//! Windows APIs: name rules, path splitting, NTSTATUS mapping, directory
//! enumeration order, FILETIME conversion and the WinFsp ABI structs.
//!
//! Keeping this OS-independent lets it be unit-tested on every platform; the
//! code that actually calls into WinFsp lives in `crate::windows`.

pub mod dirlist;
pub mod ffi;
pub mod names;
pub mod ntstatus;
pub mod time;

use crate::virtual_fs::VirtualFsAttr;
use crate::virtual_fs::inode::InodeKind;

/// Allocation granularity reported to Windows (sector 512 × 8).
pub const ALLOCATION_UNIT: u64 = 4096;

/// Translate `VirtualFs` attributes into the WinFsp file information block.
///
/// * Directories carry `FILE_ATTRIBUTE_DIRECTORY` only.
/// * Files carry `FILE_ATTRIBUTE_READONLY` on read-only mounts and
///   `FILE_ATTRIBUTE_ARCHIVE` otherwise (the NTFS default for new files).
/// * There is no creation time on the Hub; the modification time is used.
/// * The inode number is the stable file ID for the life of the mount.
pub fn file_info_for(attr: &VirtualFsAttr, read_only: bool) -> ffi::FileInfo {
    use ffi::win32::*;
    let (attributes, size) = match attr.kind {
        InodeKind::Directory => (FILE_ATTRIBUTE_DIRECTORY, 0),
        InodeKind::File | InodeKind::Symlink => {
            let attrs = if read_only {
                FILE_ATTRIBUTE_READONLY
            } else {
                FILE_ATTRIBUTE_ARCHIVE
            };
            (attrs, attr.size)
        }
    };
    let mtime = time::to_filetime(attr.mtime);
    ffi::FileInfo {
        file_attributes: attributes,
        reparse_tag: 0,
        allocation_size: size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT,
        file_size: size,
        creation_time: mtime,
        last_access_time: time::to_filetime(attr.atime),
        last_write_time: mtime,
        change_time: time::to_filetime(attr.ctime),
        index_number: attr.ino,
        hard_links: 0,
        ea_size: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn attr(kind: InodeKind, size: u64) -> VirtualFsAttr {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        VirtualFsAttr {
            ino: 42,
            size,
            blocks: 0,
            mtime: t,
            atime: t,
            ctime: t,
            kind,
            perm: 0o644,
            nlink: 1,
            uid: 0,
            gid: 0,
        }
    }

    #[test]
    fn file_info_for_files_and_dirs() {
        use ffi::win32::*;
        let ro = file_info_for(&attr(InodeKind::File, 5000), true);
        assert_eq!(ro.file_attributes, FILE_ATTRIBUTE_READONLY);
        assert_eq!(ro.file_size, 5000);
        assert_eq!(ro.allocation_size, 8192);
        assert_eq!(ro.index_number, 42);
        assert_eq!(ro.last_write_time, 133_444_736_000_000_000);
        assert_eq!(ro.creation_time, ro.last_write_time);

        let rw = file_info_for(&attr(InodeKind::File, 0), false);
        assert_eq!(rw.file_attributes, FILE_ATTRIBUTE_ARCHIVE);
        assert_eq!(rw.allocation_size, 0);

        let dir = file_info_for(&attr(InodeKind::Directory, 123), true);
        assert_eq!(dir.file_attributes, FILE_ATTRIBUTE_DIRECTORY);
        assert_eq!(dir.file_size, 0);
    }

    #[test]
    fn large_files_keep_64_bit_sizes() {
        let big = 5 * 1024 * 1024 * 1024 + 7;
        let info = file_info_for(&attr(InodeKind::File, big), true);
        assert_eq!(info.file_size, big);
        assert!(info.allocation_size >= big && info.allocation_size.is_multiple_of(ALLOCATION_UNIT));
    }
}
