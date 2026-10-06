//! Platform boundary (linux-port.md section 3). Free functions behind
//! #[cfg], re-exported so the rest of the codebase imports one path
//! regardless of OS. No dyn Platform, no traits — single small binary.
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
