//! The mount-management engine shared by the Windows CLI and the desktop
//! app: validated mount specifications, the client/host protocol, error
//! categories and exit codes, the mount registry state machine, settings and
//! profiles, credential resolution and redaction.
//!
//! Everything here is OS-independent and unit-tested on every platform; the
//! Windows transport (named pipes) and WinFsp mounter live in `crate::windows`.

pub mod auth;
pub mod config;
pub mod errors;
pub mod protocol;
pub mod redact;
pub mod registry;
pub mod spec;
