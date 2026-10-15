//! Minimal `#[repr(C)]` mirror of the WinFsp user-mode ABI
//! (`inc/winfsp/fsctl.h` and `inc/winfsp/winfsp.h`, WinFsp 2.x).
//!
//! Only the structures and entry points hf-mount uses are declared. Every
//! struct size is pinned with a compile-time assertion against the size
//! WinFsp itself freezes with `static_assert`, so a layout mistake fails the
//! build instead of corrupting memory. The definitions contain no OS types
//! and compile on every 64-bit target, which lets the size checks run in
//! Linux CI too; the DLL loader lives in `crate::windows::winfsp`.
//!
//! WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos.
//! <https://github.com/winfsp/winfsp> — used under the WinFsp FLOSS exception.

#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type NTSTATUS = i32;
pub type PWSTR = *mut u16;
pub type BOOLEAN = u8;

/// Kernel device for disk (non-network) file systems.
pub const DISK_DEVICE_NAME: &str = "WinFsp.Disk";

/// `FSP_FSCTL_VOLUME_PARAMS` (V1, 504 bytes). The C bitfields are folded into
/// `flags` / `flags2`; see [`vp`] for the bit positions.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VolumeParams {
    pub version: u16,
    pub sector_size: u16,
    pub sectors_per_allocation_unit: u16,
    pub max_component_length: u16,
    pub volume_creation_time: u64,
    pub volume_serial_number: u32,
    pub transact_timeout: u32,
    pub irp_timeout: u32,
    pub irp_capacity: u32,
    pub file_info_timeout: u32,
    pub flags: u32,
    pub prefix: [u16; 192],
    pub file_system_name: [u16; 16],
    pub flags2: u32,
    pub volume_info_timeout: u32,
    pub dir_info_timeout: u32,
    pub security_timeout: u32,
    pub stream_info_timeout: u32,
    pub ea_timeout: u32,
    pub fsext_control_code: u32,
    pub reserved32: [u32; 1],
    pub reserved64: [u64; 2],
}

impl Default for VolumeParams {
    fn default() -> Self {
        // SAFETY: all-zero is the documented "unset" state for every field.
        unsafe { std::mem::zeroed() }
    }
}

/// Bit positions of the `FSP_FSCTL_VOLUME_PARAMS` bitfields (declaration
/// order, least significant bit first, as MSVC lays out `UINT32 x:1` fields).
pub mod vp {
    pub const CASE_SENSITIVE_SEARCH: u32 = 1 << 0;
    pub const CASE_PRESERVED_NAMES: u32 = 1 << 1;
    pub const UNICODE_ON_DISK: u32 = 1 << 2;
    pub const PERSISTENT_ACLS: u32 = 1 << 3;
    pub const REPARSE_POINTS: u32 = 1 << 4;
    pub const REPARSE_POINTS_ACCESS_CHECK: u32 = 1 << 5;
    pub const NAMED_STREAMS: u32 = 1 << 6;
    pub const HARD_LINKS: u32 = 1 << 7;
    pub const EXTENDED_ATTRIBUTES: u32 = 1 << 8;
    pub const READ_ONLY_VOLUME: u32 = 1 << 9;
    pub const POST_CLEANUP_WHEN_MODIFIED_ONLY: u32 = 1 << 10;
    pub const PASS_QUERY_DIRECTORY_PATTERN: u32 = 1 << 11;
    pub const ALWAYS_USE_DOUBLE_BUFFERING: u32 = 1 << 12;
    pub const PASS_QUERY_DIRECTORY_FILE_NAME: u32 = 1 << 13;
    pub const FLUSH_AND_PURGE_ON_CLEANUP: u32 = 1 << 14;
    pub const DEVICE_CONTROL: u32 = 1 << 15;
    pub const UM_FILE_CONTEXT_IS_USER_CONTEXT2: u32 = 1 << 16;
    pub const UM_FILE_CONTEXT_IS_FULL_CONTEXT: u32 = 1 << 17;
    pub const UM_NO_REPARSE_POINTS_DIR_CHECK: u32 = 1 << 18;
    // bits 19..=23: UmReservedFlags
    pub const ALLOW_OPEN_IN_KERNEL_MODE: u32 = 1 << 24;
    pub const CASE_PRESERVED_EXTENDED_ATTRIBUTES: u32 = 1 << 25;
    pub const WSL_FEATURES: u32 = 1 << 26;
    pub const DIRECTORY_MARKER_AS_NEXT_OFFSET: u32 = 1 << 27;
    pub const REJECT_IRP_PRIOR_TO_TRANSACT0: u32 = 1 << 28;
    pub const SUPPORTS_POSIX_UNLINK_RENAME: u32 = 1 << 29;
    pub const POST_DISPOSITION_WHEN_NECESSARY_ONLY: u32 = 1 << 30;

    // flags2 (V1 fields)
    pub const VOLUME_INFO_TIMEOUT_VALID: u32 = 1 << 0;
    pub const DIR_INFO_TIMEOUT_VALID: u32 = 1 << 1;
    pub const SECURITY_TIMEOUT_VALID: u32 = 1 << 2;
    pub const STREAM_INFO_TIMEOUT_VALID: u32 = 1 << 3;
    pub const EA_TIMEOUT_VALID: u32 = 1 << 4;
}

/// `FSP_FSCTL_VOLUME_INFO` (88 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VolumeInfo {
    pub total_size: u64,
    pub free_size: u64,
    pub volume_label_length: u16,
    pub volume_label: [u16; 32],
}

/// `FSP_FSCTL_FILE_INFO` (72 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub file_attributes: u32,
    pub reparse_tag: u32,
    pub allocation_size: u64,
    pub file_size: u64,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub index_number: u64,
    pub hard_links: u32,
    pub ea_size: u32,
}

/// `FSP_FSCTL_OPEN_FILE_INFO`: the real type behind the `FileInfo` pointer
/// passed to `Create`/`Open`.
#[repr(C)]
pub struct OpenFileInfo {
    pub file_info: FileInfo,
    pub normalized_name: PWSTR,
    pub normalized_name_size: u16,
}

/// `FSP_FSCTL_DIR_INFO` header (104 bytes) followed by the UTF-16 name.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirInfo {
    pub size: u16,
    pub file_info: FileInfo,
    /// Union of `UINT64 NextOffset` and `UINT8 Padding[24]`; must be zeroed.
    pub padding: [u8; 24],
}

/// A `DirInfo` with room for a maximal (255 code unit) file name.
#[repr(C)]
pub struct DirInfoBuf {
    pub header: DirInfo,
    pub name: [u16; 256],
}

impl DirInfoBuf {
    /// Fill the buffer for `name` (≤ 255 UTF-16 code units, enforced by the
    /// caller through the name rules) and set `Size` accordingly.
    pub fn new(name: &str, file_info: FileInfo) -> Option<Box<Self>> {
        let mut buf = Box::new(Self {
            header: DirInfo {
                size: 0,
                file_info,
                padding: [0; 24],
            },
            name: [0; 256],
        });
        let mut len = 0usize;
        for unit in name.encode_utf16() {
            if len >= 255 {
                return None;
            }
            buf.name[len] = unit;
            len += 1;
        }
        buf.header.size = (std::mem::size_of::<DirInfo>() + len * 2) as u16;
        Some(buf)
    }
}

/// Opaque `FSP_FILE_SYSTEM`.
#[repr(C)]
pub struct FspFileSystem {
    _private: [u8; 0],
}

/// The stable prefix of `FSP_FILE_SYSTEM` (`UINT16 Version; PVOID UserContext; …`).
#[repr(C)]
pub struct FspFileSystemPrefix {
    pub version: u16,
    pub user_context: *mut c_void,
}

pub type UnusedFn = unsafe extern "system" fn();
pub type GetVolumeInfoFn = unsafe extern "system" fn(*mut FspFileSystem, *mut VolumeInfo) -> NTSTATUS;
pub type SetVolumeLabelFn = unsafe extern "system" fn(*mut FspFileSystem, PWSTR, *mut VolumeInfo) -> NTSTATUS;
pub type GetSecurityByNameFn =
    unsafe extern "system" fn(*mut FspFileSystem, PWSTR, *mut u32, *mut c_void, *mut usize) -> NTSTATUS;
pub type CreateFn = unsafe extern "system" fn(
    *mut FspFileSystem,
    PWSTR,
    u32,
    u32,
    u32,
    *mut c_void,
    u64,
    *mut *mut c_void,
    *mut FileInfo,
) -> NTSTATUS;
pub type OpenFn =
    unsafe extern "system" fn(*mut FspFileSystem, PWSTR, u32, u32, *mut *mut c_void, *mut FileInfo) -> NTSTATUS;
pub type OverwriteFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, u32, BOOLEAN, u64, *mut FileInfo) -> NTSTATUS;
pub type CleanupFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, PWSTR, u32);
pub type CloseFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void);
pub type ReadFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, *mut c_void, u64, u32, *mut u32) -> NTSTATUS;
pub type WriteFn = unsafe extern "system" fn(
    *mut FspFileSystem,
    *mut c_void,
    *mut c_void,
    u64,
    u32,
    BOOLEAN,
    BOOLEAN,
    *mut u32,
    *mut FileInfo,
) -> NTSTATUS;
pub type FlushFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, *mut FileInfo) -> NTSTATUS;
pub type GetFileInfoFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, *mut FileInfo) -> NTSTATUS;
pub type SetBasicInfoFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, u32, u64, u64, u64, u64, *mut FileInfo) -> NTSTATUS;
pub type SetFileSizeFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, u64, BOOLEAN, *mut FileInfo) -> NTSTATUS;
pub type CanDeleteFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, PWSTR) -> NTSTATUS;
pub type RenameFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, PWSTR, PWSTR, BOOLEAN) -> NTSTATUS;
pub type GetSecurityFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, *mut c_void, *mut usize) -> NTSTATUS;
pub type SetSecurityFn = unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, u32, *mut c_void) -> NTSTATUS;
pub type ReadDirectoryFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, PWSTR, PWSTR, *mut c_void, u32, *mut u32) -> NTSTATUS;
pub type GetDirInfoByNameFn =
    unsafe extern "system" fn(*mut FspFileSystem, *mut c_void, PWSTR, *mut DirInfo) -> NTSTATUS;
pub type DispatcherStoppedFn = unsafe extern "system" fn(*mut FspFileSystem, BOOLEAN);

/// `FSP_FILE_SYSTEM_INTERFACE`: exactly 64 function pointer slots.
#[repr(C)]
pub struct FileSystemInterface {
    pub get_volume_info: Option<GetVolumeInfoFn>,
    pub set_volume_label: Option<SetVolumeLabelFn>,
    pub get_security_by_name: Option<GetSecurityByNameFn>,
    pub create: Option<CreateFn>,
    pub open: Option<OpenFn>,
    pub overwrite: Option<OverwriteFn>,
    pub cleanup: Option<CleanupFn>,
    pub close: Option<CloseFn>,
    pub read: Option<ReadFn>,
    pub write: Option<WriteFn>,
    pub flush: Option<FlushFn>,
    pub get_file_info: Option<GetFileInfoFn>,
    pub set_basic_info: Option<SetBasicInfoFn>,
    pub set_file_size: Option<SetFileSizeFn>,
    pub can_delete: Option<CanDeleteFn>,
    pub rename: Option<RenameFn>,
    pub get_security: Option<GetSecurityFn>,
    pub set_security: Option<SetSecurityFn>,
    pub read_directory: Option<ReadDirectoryFn>,
    pub resolve_reparse_points: Option<UnusedFn>,
    pub get_reparse_point: Option<UnusedFn>,
    pub set_reparse_point: Option<UnusedFn>,
    pub delete_reparse_point: Option<UnusedFn>,
    pub get_stream_info: Option<UnusedFn>,
    pub get_dir_info_by_name: Option<GetDirInfoByNameFn>,
    pub control: Option<UnusedFn>,
    pub set_delete: Option<UnusedFn>,
    pub create_ex: Option<UnusedFn>,
    pub overwrite_ex: Option<UnusedFn>,
    pub get_ea: Option<UnusedFn>,
    pub set_ea: Option<UnusedFn>,
    pub obsolete0: Option<UnusedFn>,
    pub dispatcher_stopped: Option<DispatcherStoppedFn>,
    pub reserved: [Option<UnusedFn>; 31],
}

impl Default for FileSystemInterface {
    fn default() -> Self {
        // SAFETY: every field is an `Option<fn>`; all-zero is `None`.
        unsafe { std::mem::zeroed() }
    }
}

/// `FspCleanup*` flags passed to `Cleanup`.
pub mod cleanup {
    pub const DELETE: u32 = 0x01;
    pub const SET_ALLOCATION_SIZE: u32 = 0x02;
    pub const SET_ARCHIVE_BIT: u32 = 0x10;
    pub const SET_LAST_ACCESS_TIME: u32 = 0x20;
    pub const SET_LAST_WRITE_TIME: u32 = 0x40;
    pub const SET_CHANGE_TIME: u32 = 0x80;
}

/// Win32 constants the adapter needs (also in windows-sys; repeated here so
/// this module stays OS-independent).
pub mod win32 {
    pub const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
    pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
    pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x0000_0020;
    pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    pub const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

    pub const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
    pub const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;

    pub const FILE_READ_DATA: u32 = 0x0001;
    pub const FILE_WRITE_DATA: u32 = 0x0002;
    pub const FILE_APPEND_DATA: u32 = 0x0004;
}

const _: () = {
    assert!(std::mem::size_of::<VolumeParams>() == 504);
    assert!(std::mem::size_of::<VolumeInfo>() == 88);
    assert!(std::mem::size_of::<FileInfo>() == 72);
    assert!(std::mem::size_of::<DirInfo>() == 104);
    assert!(std::mem::size_of::<FileSystemInterface>() == 64 * std::mem::size_of::<usize>());
    assert!(std::mem::offset_of!(VolumeParams, file_info_timeout) == 32);
    assert!(std::mem::offset_of!(VolumeParams, flags) == 36);
    assert!(std::mem::offset_of!(VolumeParams, prefix) == 40);
    assert!(std::mem::offset_of!(VolumeParams, file_system_name) == 424);
    assert!(std::mem::offset_of!(VolumeParams, flags2) == 456);
    assert!(std::mem::offset_of!(DirInfo, file_info) == 8);
    assert!(std::mem::offset_of!(DirInfoBuf, name) == 104);
    assert!(std::mem::offset_of!(FspFileSystemPrefix, user_context) == 8);
    assert!(std::mem::offset_of!(FileSystemInterface, read_directory) == 18 * std::mem::size_of::<usize>());
    assert!(std::mem::offset_of!(FileSystemInterface, get_dir_info_by_name) == 24 * std::mem::size_of::<usize>());
    assert!(std::mem::offset_of!(FileSystemInterface, dispatcher_stopped) == 32 * std::mem::size_of::<usize>());
};

/// Copy `s` into a fixed UTF-16 field, NUL-terminated and truncated to fit.
pub fn copy_wide(dst: &mut [u16], s: &str) {
    let max = dst.len().saturating_sub(1);
    let mut n = 0;
    for unit in s.encode_utf16().take(max) {
        dst[n] = unit;
        n += 1;
    }
    if n < dst.len() {
        dst[n] = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_info_buf_size_counts_utf16_units() {
        let buf = DirInfoBuf::new("a😀", FileInfo::default()).unwrap();
        assert_eq!(buf.header.size as usize, 104 + 3 * 2);
        assert_eq!(&buf.name[..3], &[0x61, 0xD83D, 0xDE00]);
        assert!(DirInfoBuf::new(&"x".repeat(256), FileInfo::default()).is_none());
        assert!(DirInfoBuf::new(&"x".repeat(255), FileInfo::default()).is_some());
    }

    #[test]
    fn copy_wide_truncates_and_terminates() {
        let mut dst = [0xFFFFu16; 4];
        copy_wide(&mut dst, "abcdef");
        assert_eq!(dst, [0x61, 0x62, 0x63, 0]);
        let mut dst = [0xFFFFu16; 8];
        copy_wide(&mut dst, "hf");
        assert_eq!(&dst[..3], &[0x68, 0x66, 0]);
    }
}
