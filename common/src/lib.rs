//! Everything the agent and the controller must agree on: how a session file is named, what is
//! inside it, and how a genuine reading becomes a fake one.

pub mod config;
// The kernel's vDSO, which the genuine clock reads through and the agent's synthetic one imitates
// on Linux, and the program's own headers, which FreeBSD scans to find a Go runtime's clock
// routines.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod elf;
pub mod hook;
pub mod log;
pub mod platform;
pub mod session;
pub mod sync;
pub mod time;
