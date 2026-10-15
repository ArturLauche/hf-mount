//! Process identity and security descriptors.

use std::ffi::c_void;
use std::io;

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::util::{OwnedHandle, from_wide_ptr, last_error, wide};

fn process_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: valid process handle and out pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    Ok(OwnedHandle(token))
}

/// Token information in an 8-byte aligned buffer (so the result can be cast
/// to the matching `TOKEN_*` struct; embedded pointers refer into the buffer).
fn token_info(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: size query with a null buffer.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(last_error());
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer holds at least `len` bytes.
    if unsafe { GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(last_error());
    }
    Ok(buf)
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let buf = token_info(token, TokenUser)?;
    // SAFETY: the aligned buffer holds a TOKEN_USER; its SID points inside `buf`,
    // which outlives the conversion below.
    let user = unsafe { &*(buf.as_ptr().cast::<TOKEN_USER>()) };
    sid_to_string(user.User.Sid)
}

fn sid_to_string(sid: *mut c_void) -> io::Result<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID pointer; the OS allocates `out` with LocalAlloc.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(last_error());
    }
    let s = unsafe { from_wide_ptr(out, 256) };
    unsafe { LocalFree(out.cast()) };
    s.ok_or_else(|| io::Error::other("SID string is not valid UTF-16"))
}

/// String SID (`S-1-5-21-…`) of the user running this process.
pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: pseudo-handle, no close needed.
    let token = process_token(unsafe { GetCurrentProcess() })?;
    token_user_sid(token.0)
}

/// String SID of the user owning process `pid`.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: plain Win32 call; the handle is owned below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(last_error());
    }
    let process = OwnedHandle(process);
    let token = process_token(process.0)?;
    token_user_sid(token.0)
}

/// Whether this process runs with an elevated (administrator) token.
pub fn is_elevated() -> bool {
    let Ok(token) = process_token(unsafe { GetCurrentProcess() }) else {
        return false;
    };
    let Ok(buf) = token_info(token.0, TokenElevation) else {
        return false;
    };
    // SAFETY: the buffer holds a TOKEN_ELEVATION.
    unsafe { &*(buf.as_ptr().cast::<TOKEN_ELEVATION>()) }.TokenIsElevated != 0
}

/// Terminal Services session of this process (drive letters are scoped to
/// the logon session, so the control pipe names include it).
pub fn current_session_id() -> u32 {
    let mut session = 0u32;
    // SAFETY: valid out pointer.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
        return 0;
    }
    session
}

/// Session of process `pid`, if it can be queried.
pub fn process_session_id(pid: u32) -> Option<u32> {
    let mut session = 0u32;
    // SAFETY: valid out pointer.
    (unsafe { ProcessIdToSessionId(pid, &mut session) } != 0).then_some(session)
}

/// A self-relative security descriptor built from SDDL.
pub struct SecurityDescriptor {
    bytes: Vec<u8>,
}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut sd: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: valid NUL-terminated string; the OS allocates `sd` with LocalAlloc.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, &mut len)
        } == 0
        {
            return Err(last_error());
        }
        // SAFETY: `sd` points to `len` bytes of a self-relative descriptor.
        let bytes = unsafe { std::slice::from_raw_parts(sd.cast::<u8>(), len as usize) }.to_vec();
        unsafe { LocalFree(sd) };
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Pointer suitable for `SECURITY_ATTRIBUTES::lpSecurityDescriptor`. The
    /// descriptor is self-relative, so a plain byte copy stays valid.
    pub fn as_ptr(&self) -> *mut c_void {
        self.bytes.as_ptr() as *mut c_void
    }
}

/// SDDL for files and directories on a mounted volume: the mounting user,
/// SYSTEM and Administrators get full control; everyone else (including
/// AppContainer apps such as Photos) may read and execute. Writes are still
/// refused for read-only volumes by the `ReadOnlyVolume` flag.
pub fn volume_sddl(user_sid: &str) -> String {
    format!(
        "O:{user_sid}G:{user_sid}D:P(A;;FA;;;{user_sid})(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;WD)(A;;0x1200a9;;;AC)"
    )
}

/// SDDL for the control pipes: only the owning user, local connections only
/// (remote clients are additionally rejected with `PIPE_REJECT_REMOTE_CLIENTS`).
pub fn pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})")
}
