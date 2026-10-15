//! Native Windows support: WinFsp volumes, the per-user mount supervisor,
//! its control pipe, credentials and the `hf-mount.exe` command line.
//!
//! Platform-independent pieces (name rules, NTSTATUS mapping, ABI structs)
//! live in [`crate::winfs`] so they are tested on every platform.

pub mod cli;
pub mod client;
mod console;
pub mod credman;
pub mod drives;
pub mod host;
pub mod mounter;
pub mod pipe;
pub mod process;
pub mod security;
pub mod util;
pub mod winfsp;
pub mod winfsp_fs;

#[cfg(test)]
mod mount_tests;
