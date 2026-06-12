//! Helpers for Windows drive-letter mount targets and System32 tools.
//!
//! Shared by the NFS backend, mount setup, and the GUI so the parsing rules
//! stay in one place. The module compiles on every platform — the functions
//! are pure (or env-var based), which lets Linux CI cover the logic — but the
//! semantics only matter on Windows.

use std::path::PathBuf;

/// Parse a bare drive-letter target such as `Z:`, `Z:\` or `z:/`.
///
/// Returns the drive letter for drive-letter targets and `None` for anything
/// longer (e.g. `C:\hf-mounts\repo`), which is treated as a directory path.
pub fn drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() || chars.next() != Some(':') {
        return None;
    }
    match (chars.next(), chars.next()) {
        (None, None) => Some(drive),
        (Some('\\' | '/'), None) => Some(drive),
        _ => None,
    }
}

/// Absolute path to a System32 executable (`mount.exe`, `umount.exe`, ...).
///
/// Resolving through `%SystemRoot%` avoids PATH hijacking for the privileged
/// helper tools the NFS backend shells out to.
pub fn system32_exe(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(name)
}

/// Drive letters that are not currently assigned, from a `GetLogicalDrives`
/// style bitmask (bit 0 = `A:`, bit 25 = `Z:`).
///
/// Returned in reverse alphabetical order (`Z`, `Y`, ...) because high letters
/// are the conventional choice for removable/network mounts and the least
/// likely to collide with local disks.
pub fn free_drive_letters(assigned_mask: u32) -> Vec<char> {
    ('D'..='Z')
        .rev()
        .filter(|letter| assigned_mask & (1 << (*letter as u8 - b'A')) == 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letter_accepts_bare_targets() {
        assert_eq!(drive_letter("Z:"), Some('Z'));
        assert_eq!(drive_letter("Z:\\"), Some('Z'));
        assert_eq!(drive_letter("z:/"), Some('z'));
    }

    #[test]
    fn drive_letter_rejects_paths_and_garbage() {
        assert_eq!(drive_letter(r"C:\hf-mounts\repo"), None);
        assert_eq!(drive_letter("Z:x"), None);
        assert_eq!(drive_letter("ZZ:"), None);
        assert_eq!(drive_letter("/tmp/mount"), None);
        assert_eq!(drive_letter(""), None);
        assert_eq!(drive_letter("1:"), None);
    }

    #[test]
    fn free_drive_letters_skips_assigned_bits_and_reserved_letters() {
        // C: and D: assigned -> D excluded, A/B never offered, Z first.
        let mask = (1 << 2) | (1 << 3);
        let free = free_drive_letters(mask);
        assert_eq!(free.first(), Some(&'Z'));
        assert!(!free.contains(&'D'));
        assert!(!free.contains(&'A'));
        assert!(!free.contains(&'B'));
        assert!(!free.contains(&'C'));

        // All assigned -> nothing free.
        assert!(free_drive_letters(u32::MAX).is_empty());
    }
}
