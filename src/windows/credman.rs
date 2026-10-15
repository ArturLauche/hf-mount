//! Hugging Face token storage in Windows Credential Manager.
//!
//! Stored as a generic credential (DPAPI-protected, per user) named
//! `hf-mount:<hub host>`. The account name is kept in the credential's user
//! name field so the UI can show who is signed in without reading the secret.

use std::io;

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};

use super::util::{from_wide_ptr, last_error, wide};
use crate::manager::auth::Secret;

/// Credential target for a Hub endpoint (`hf-mount:huggingface.co`).
pub fn target_name(endpoint: &str) -> String {
    let host = endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', '?'])
        .next()
        .unwrap_or("huggingface.co");
    format!("hf-mount:{}", if host.is_empty() { "huggingface.co" } else { host })
}

/// Store `token` (replacing any previous one) with an optional account label.
pub fn store(endpoint: &str, token: &str, account: Option<&str>) -> io::Result<()> {
    let target = wide(target_name(endpoint));
    let user = wide(account.unwrap_or("Hugging Face token"));
    let comment = wide("Hugging Face access token used by hf-mount");
    let blob = token.trim().as_bytes();
    let cred = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_ptr() as *mut u16,
        Comment: comment.as_ptr() as *mut u16,
        LastWritten: FILETIME::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: user.as_ptr() as *mut u16,
    };
    // SAFETY: all pointers are valid for the duration of the call.
    if unsafe { CredWriteW(&cred, 0) } == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// The stored token and account label, if any.
pub fn read(endpoint: &str) -> io::Result<Option<(Secret, Option<String>)>> {
    let target = wide(target_name(endpoint));
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: valid target; the OS allocates `cred`.
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) } == 0 {
        let err = last_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(None);
        }
        return Err(err);
    }
    // SAFETY: CredReadW succeeded; fields are valid until CredFree.
    let result = unsafe {
        let c = &*cred;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let token = String::from_utf8(blob.to_vec()).ok().and_then(Secret::new);
        let account = from_wide_ptr(c.UserName, 512).filter(|a| a != "Hugging Face token");
        token.map(|t| (t, account))
    };
    unsafe { CredFree(cred as *const _) };
    Ok(result)
}

/// Remove the stored token. Returns whether one existed.
pub fn delete(endpoint: &str) -> io::Result<bool> {
    let target = wide(target_name(endpoint));
    // SAFETY: valid target name.
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let err = last_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(false);
        }
        return Err(err);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_names() {
        assert_eq!(target_name("https://huggingface.co"), "hf-mount:huggingface.co");
        assert_eq!(target_name("https://hub.example.com/"), "hf-mount:hub.example.com");
        assert_eq!(target_name(""), "hf-mount:huggingface.co");
    }

    /// Exercises the real Credential Manager under a test-only endpoint.
    #[test]
    fn store_read_delete_roundtrip() {
        let endpoint = format!("https://hf-mount-test-{}.invalid", std::process::id());
        let _ = delete(&endpoint);
        assert!(read(&endpoint).unwrap().is_none());
        store(&endpoint, "hf_testtoken_roundtrip", Some("tester")).unwrap();
        let (secret, account) = read(&endpoint).unwrap().unwrap();
        assert_eq!(secret.expose(), "hf_testtoken_roundtrip");
        assert_eq!(account.as_deref(), Some("tester"));
        assert!(delete(&endpoint).unwrap());
        assert!(!delete(&endpoint).unwrap());
        assert!(read(&endpoint).unwrap().is_none());
    }
}
