//! Drive-letter availability.

use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, QueryDosDeviceW};

/// Letters offered for new mounts. A–C are left alone (floppy legacy and the
/// system drive).
pub const CANDIDATE_LETTERS: std::ops::RangeInclusive<char> = 'D'..='Z';

/// Whether `letter` is unused in this logon session: no volume reported by
/// `GetLogicalDrives` and no DOS device mapping (`subst`, network drive,
/// another hf-mount) for `X:`.
pub fn is_letter_free(letter: char) -> bool {
    let letter = letter.to_ascii_uppercase();
    if !letter.is_ascii_uppercase() {
        return false;
    }
    // SAFETY: plain Win32 call.
    free_in_mask(letter, unsafe { GetLogicalDrives() })
}

fn free_in_mask(letter: char, mask: u32) -> bool {
    if mask & (1 << (letter as u32 - 'A' as u32)) != 0 {
        return false;
    }
    let device = [letter as u16, b':' as u16, 0];
    let mut target = [0u16; 512];
    // SAFETY: valid buffers; returns 0 when the name has no mapping.
    let n = unsafe { QueryDosDeviceW(device.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
    n == 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
}

/// Free letters among [`CANDIDATE_LETTERS`], in order.
pub fn free_letters() -> Vec<char> {
    // One snapshot for the whole list. Mount itself still checks the live letter.
    let mask = unsafe { GetLogicalDrives() };
    CANDIDATE_LETTERS.filter(|&c| free_in_mask(c, mask)).collect()
}

/// Whether a drive root (e.g. `H:\`) currently exists.
pub fn drive_exists(letter: char) -> bool {
    std::path::Path::new(&format!("{}:\\", letter.to_ascii_uppercase())).exists()
}
