pub mod cached_xet_client;
#[cfg(unix)]
pub mod daemon;
#[cfg(not(unix))]
pub mod daemon {
    /// Windows stub used by foreground backends. The background daemon
    /// controller is Unix-only; Windows users run the NFS backend directly.
    pub struct DaemonGuard;

    impl DaemonGuard {
        pub fn from_env() -> Option<Self> {
            None
        }

        pub fn notify_ready(&mut self) {}
    }
}
pub mod error;
pub mod file_cache;
#[cfg(all(unix, feature = "fuse"))]
pub mod fuse;
pub mod hub_api;
#[cfg(feature = "nfs")]
pub mod nfs;
pub mod overlay;
pub mod setup;
pub mod virtual_fs;
pub mod windows;
pub mod xet;

#[cfg(test)]
pub(crate) mod test_mocks;
