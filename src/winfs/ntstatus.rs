//! NTSTATUS values used by the WinFsp adapter and the errno → NTSTATUS
//! translation for errors reported by `VirtualFs`.
//!
//! `VirtualFs` reports `libc` errno constants. On Windows those are the CRT
//! values (e.g. `ENOTEMPTY` = 41, not Linux's 39); the table below names the
//! constants, so it is correct on every target.

pub type NtStatus = i32;

macro_rules! ntstatus {
    ($($name:ident = $value:expr;)*) => {
        $(pub const $name: NtStatus = $value as u32 as i32;)*
    };
}

ntstatus! {
    STATUS_SUCCESS = 0x0000_0000u32;
    STATUS_PENDING = 0x0000_0103u32;
    STATUS_REPARSE = 0x0000_0104u32;
    STATUS_BUFFER_OVERFLOW = 0x8000_0005u32;
    STATUS_NO_MORE_FILES = 0x8000_0006u32;
    STATUS_UNSUCCESSFUL = 0xC000_0001u32;
    STATUS_NOT_IMPLEMENTED = 0xC000_0002u32;
    STATUS_INVALID_HANDLE = 0xC000_0008u32;
    STATUS_INVALID_PARAMETER = 0xC000_000Du32;
    STATUS_NO_SUCH_DEVICE = 0xC000_000Eu32;
    STATUS_INVALID_DEVICE_REQUEST = 0xC000_0010u32;
    STATUS_END_OF_FILE = 0xC000_0011u32;
    STATUS_ACCESS_DENIED = 0xC000_0022u32;
    STATUS_BUFFER_TOO_SMALL = 0xC000_0023u32;
    STATUS_OBJECT_NAME_INVALID = 0xC000_0033u32;
    STATUS_OBJECT_NAME_NOT_FOUND = 0xC000_0034u32;
    STATUS_OBJECT_NAME_COLLISION = 0xC000_0035u32;
    STATUS_OBJECT_PATH_NOT_FOUND = 0xC000_003Au32;
    STATUS_SHARING_VIOLATION = 0xC000_0043u32;
    STATUS_DELETE_PENDING = 0xC000_0056u32;
    STATUS_DISK_FULL = 0xC000_007Fu32;
    STATUS_INSUFFICIENT_RESOURCES = 0xC000_009Au32;
    STATUS_MEDIA_WRITE_PROTECTED = 0xC000_00A2u32;
    STATUS_IO_TIMEOUT = 0xC000_00B5u32;
    STATUS_FILE_IS_A_DIRECTORY = 0xC000_00BAu32;
    STATUS_NOT_SUPPORTED = 0xC000_00BBu32;
    STATUS_NETWORK_BUSY = 0xC000_00BFu32;
    STATUS_NOT_SAME_DEVICE = 0xC000_00D4u32;
    STATUS_UNEXPECTED_IO_ERROR = 0xC000_00E9u32;
    STATUS_DIRECTORY_NOT_EMPTY = 0xC000_0101u32;
    STATUS_NOT_A_DIRECTORY = 0xC000_0103u32;
    STATUS_CANCELLED = 0xC000_0120u32;
    STATUS_CANNOT_DELETE = 0xC000_0121u32;
    STATUS_DEVICE_BUSY = 0x8000_0011u32;
    STATUS_DLL_NOT_FOUND = 0xC000_0135u32;
    STATUS_NETWORK_UNREACHABLE = 0xC000_023Cu32;
    STATUS_FILE_TOO_LARGE = 0xC000_0904u32;
}

/// Whether `status` is a success code (`NT_SUCCESS`).
pub fn nt_success(status: NtStatus) -> bool {
    status >= 0
}

/// Translate a `VirtualFs` errno into the NTSTATUS a Windows caller sees.
///
/// The Hub-facing errnos are chosen so the Win32 message is meaningful:
/// `EAGAIN` (rate limit, 5xx, transport hiccup) becomes "The network is busy",
/// `EIO` becomes "An unexpected network error occurred", quota rejects become
/// "There is not enough space on the disk". Nothing maps to success.
pub fn errno_to_ntstatus(errno: i32) -> NtStatus {
    match errno {
        libc::ENOENT => STATUS_OBJECT_NAME_NOT_FOUND,
        libc::ENOTDIR => STATUS_NOT_A_DIRECTORY,
        libc::EISDIR => STATUS_FILE_IS_A_DIRECTORY,
        libc::EEXIST => STATUS_OBJECT_NAME_COLLISION,
        libc::ENOTEMPTY => STATUS_DIRECTORY_NOT_EMPTY,
        libc::EROFS => STATUS_MEDIA_WRITE_PROTECTED,
        libc::EACCES | libc::EPERM => STATUS_ACCESS_DENIED,
        libc::ENOSPC => STATUS_DISK_FULL,
        libc::EFBIG => STATUS_FILE_TOO_LARGE,
        libc::EAGAIN => STATUS_NETWORK_BUSY,
        libc::ETIMEDOUT => STATUS_IO_TIMEOUT,
        libc::EBADF => STATUS_INVALID_HANDLE,
        libc::EINVAL => STATUS_INVALID_PARAMETER,
        libc::ENAMETOOLONG => STATUS_OBJECT_NAME_INVALID,
        libc::ENOTSUP => STATUS_NOT_SUPPORTED,
        libc::ENOSYS => STATUS_NOT_IMPLEMENTED,
        libc::EXDEV => STATUS_NOT_SAME_DEVICE,
        libc::EBUSY => STATUS_SHARING_VIOLATION,
        libc::ENOMEM => STATUS_INSUFFICIENT_RESOURCES,
        libc::ECANCELED => STATUS_CANCELLED,
        _ => STATUS_UNEXPECTED_IO_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_have_expected_bit_patterns() {
        assert_eq!(STATUS_OBJECT_NAME_NOT_FOUND as u32, 0xC000_0034);
        assert_eq!(STATUS_BUFFER_OVERFLOW as u32, 0x8000_0005);
        assert!(nt_success(STATUS_SUCCESS));
        assert!(nt_success(STATUS_PENDING));
        assert!(!nt_success(STATUS_BUFFER_OVERFLOW));
        assert!(!nt_success(STATUS_ACCESS_DENIED));
    }

    #[test]
    fn maps_errnos() {
        assert_eq!(errno_to_ntstatus(libc::ENOENT), STATUS_OBJECT_NAME_NOT_FOUND);
        assert_eq!(errno_to_ntstatus(libc::EROFS), STATUS_MEDIA_WRITE_PROTECTED);
        assert_eq!(errno_to_ntstatus(libc::ENOSPC), STATUS_DISK_FULL);
        assert_eq!(errno_to_ntstatus(libc::EACCES), STATUS_ACCESS_DENIED);
        assert_eq!(errno_to_ntstatus(libc::EPERM), STATUS_ACCESS_DENIED);
        assert_eq!(errno_to_ntstatus(libc::ENOTEMPTY), STATUS_DIRECTORY_NOT_EMPTY);
        assert_eq!(errno_to_ntstatus(libc::EEXIST), STATUS_OBJECT_NAME_COLLISION);
        assert_eq!(errno_to_ntstatus(libc::EAGAIN), STATUS_NETWORK_BUSY);
        assert_eq!(errno_to_ntstatus(libc::EIO), STATUS_UNEXPECTED_IO_ERROR);
        assert_eq!(errno_to_ntstatus(12345), STATUS_UNEXPECTED_IO_ERROR);
    }

    #[test]
    fn no_errno_maps_to_success() {
        for errno in 0..200 {
            assert!(!nt_success(errno_to_ntstatus(errno)), "errno {errno} mapped to success");
        }
    }

    #[test]
    fn hub_error_errnos_map_to_meaningful_statuses() {
        use crate::error::Error;
        assert_eq!(
            errno_to_ntstatus(Error::hub_status(507, "quota").to_errno()),
            STATUS_DISK_FULL
        );
        assert_eq!(
            errno_to_ntstatus(Error::hub_status(403, "forbidden").to_errno()),
            STATUS_ACCESS_DENIED
        );
        assert_eq!(
            errno_to_ntstatus(Error::hub_status(429, "slow down").to_errno()),
            STATUS_NETWORK_BUSY
        );
    }
}
