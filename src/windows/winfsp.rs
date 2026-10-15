//! Run-time loading of the WinFsp user-mode DLL.
//!
//! The DLL is found through `HKLM\SOFTWARE\WOW6432Node\WinFsp\InstallDir`
//! (written by the WinFsp installer, writable by administrators only) and
//! loaded by absolute path. Unlike WinFsp's stock `FspLoad` helper we never
//! fall back to a bare `LoadLibrary("winfsp-x64.dll")`, whose search order
//! includes the current directory.
//!
//! WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos.
//! <https://github.com/winfsp/winfsp>

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

use super::util::wide;
use crate::winfs::ffi::{BOOLEAN, DirInfo, FileSystemInterface, FspFileSystem, NTSTATUS, PWSTR, VolumeParams};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\WinFsp";
const DLL_NAME: &str = "winfsp-x64.dll";

/// Why WinFsp could not be used.
#[derive(Debug, Clone)]
pub enum LoadError {
    /// No WinFsp installation registered on this machine.
    NotInstalled,
    /// Registered, but the DLL could not be loaded (corrupt/partial install).
    DllLoadFailed { path: PathBuf, error: String },
    /// The DLL lacks an entry point we need (too old).
    MissingSymbol(&'static str),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotInstalled => f.write_str(
                "WinFsp is not installed. Install it from https://winfsp.dev/rel/ \
                 (or `winget install WinFsp.WinFsp`), then try again",
            ),
            LoadError::DllLoadFailed { path, error } => {
                write!(
                    f,
                    "WinFsp is installed but {} could not be loaded: {error}. Reinstall WinFsp",
                    path.display()
                )
            }
            LoadError::MissingSymbol(name) => {
                write!(
                    f,
                    "the installed WinFsp is too old (missing {name}); install WinFsp 2.0 or newer"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

type FileSystemCreateFn = unsafe extern "system" fn(
    PWSTR,
    *const VolumeParams,
    *const FileSystemInterface,
    *mut *mut FspFileSystem,
) -> NTSTATUS;
type FileSystemFn = unsafe extern "system" fn(*mut FspFileSystem);
type SetMountPointFn = unsafe extern "system" fn(*mut FspFileSystem, PWSTR) -> NTSTATUS;
type StartDispatcherFn = unsafe extern "system" fn(*mut FspFileSystem, u32) -> NTSTATUS;
type AddDirInfoFn = unsafe extern "system" fn(*mut DirInfo, *mut c_void, u32, *mut u32) -> BOOLEAN;
type SetDebugLogFn = unsafe extern "system" fn(*mut FspFileSystem, u32);
type VersionFn = unsafe extern "system" fn(*mut u32) -> NTSTATUS;
type RawProc = unsafe extern "system" fn() -> isize;

/// Entry points resolved from `winfsp-x64.dll`.
pub struct WinFspApi {
    pub install_dir: PathBuf,
    /// Packed DLL version (`major << 16 | minor`), when `FspVersion` reports one.
    pub version: Option<u32>,
    pub file_system_create: FileSystemCreateFn,
    pub file_system_delete: FileSystemFn,
    pub set_mount_point: SetMountPointFn,
    pub remove_mount_point: FileSystemFn,
    pub start_dispatcher: StartDispatcherFn,
    pub stop_dispatcher: FileSystemFn,
    pub add_dir_info: AddDirInfoFn,
    pub set_debug_log: SetDebugLogFn,
}

impl WinFspApi {
    /// Human-readable version, e.g. `2.1`.
    pub fn version_string(&self) -> Option<String> {
        self.version.map(|v| format!("{}.{}", v >> 16, v & 0xFFFF))
    }
}

// SAFETY: plain function pointers into a DLL that stays loaded for the life
// of the process.
unsafe impl Send for WinFspApi {}
unsafe impl Sync for WinFspApi {}

/// WinFsp install directory from the registry, if WinFsp is installed.
pub fn install_dir() -> Option<PathBuf> {
    let key = wide(REGISTRY_KEY);
    let value = wide("InstallDir");
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: valid buffers; RRF_RT_REG_SZ guarantees NUL termination.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc != 0 {
        return None;
    }
    let units = (size as usize / 2).saturating_sub(1);
    let dir = String::from_utf16(&buf[..units]).ok()?;
    let dir = PathBuf::from(dir.trim_end_matches('\0'));
    (!dir.as_os_str().is_empty()).then_some(dir)
}

fn load_uncached() -> Result<WinFspApi, LoadError> {
    let install_dir = install_dir().ok_or(LoadError::NotInstalled)?;
    let dll = install_dir.join("bin").join(DLL_NAME);
    if !dll.is_file() {
        return Err(LoadError::DllLoadFailed {
            path: dll,
            error: "file not found".to_string(),
        });
    }
    let path = wide(&dll);
    // SAFETY: absolute path; LOAD_WITH_ALTERED_SEARCH_PATH resolves the DLL's
    // own dependencies from its directory.
    let module = unsafe { LoadLibraryExW(path.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
    if module.is_null() {
        return Err(LoadError::DllLoadFailed {
            path: dll,
            error: std::io::Error::last_os_error().to_string(),
        });
    }

    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            // SAFETY: NUL-terminated ASCII symbol name.
            let p = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
            match p {
                // SAFETY: the symbol has the signature declared in winfsp.h.
                Some(f) => unsafe { std::mem::transmute::<RawProc, $ty>(f) },
                None => return Err(LoadError::MissingSymbol($name)),
            }
        }};
    }

    let version = {
        // SAFETY: optional symbol; signature NTSTATUS FspVersion(PUINT32).
        let p = unsafe { GetProcAddress(module, c"FspVersion".as_ptr().cast()) };
        p.and_then(|f| {
            let f = unsafe { std::mem::transmute::<RawProc, VersionFn>(f) };
            let mut v = 0u32;
            (unsafe { f(&mut v) } >= 0).then_some(v)
        })
    };

    Ok(WinFspApi {
        install_dir,
        version,
        file_system_create: sym!("FspFileSystemCreate", FileSystemCreateFn),
        file_system_delete: sym!("FspFileSystemDelete", FileSystemFn),
        set_mount_point: sym!("FspFileSystemSetMountPoint", SetMountPointFn),
        remove_mount_point: sym!("FspFileSystemRemoveMountPoint", FileSystemFn),
        start_dispatcher: sym!("FspFileSystemStartDispatcher", StartDispatcherFn),
        stop_dispatcher: sym!("FspFileSystemStopDispatcher", FileSystemFn),
        add_dir_info: sym!("FspFileSystemAddDirInfo", AddDirInfoFn),
        set_debug_log: sym!("FspFileSystemSetDebugLogF", SetDebugLogFn),
    })
}

/// Load WinFsp once per process. Only success is cached, so installing
/// WinFsp while the app or supervisor runs is picked up on the next call.
pub fn api() -> Result<&'static WinFspApi, LoadError> {
    static API: OnceLock<&'static WinFspApi> = OnceLock::new();
    static LOADING: Mutex<()> = Mutex::new(());
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let _guard = LOADING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let api: &'static WinFspApi = Box::leak(Box::new(load_uncached()?));
    let _ = API.set(api);
    Ok(api)
}
