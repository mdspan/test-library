//! Everything the agent and the controller must agree on: how a session file is named, what is
//! inside it, and how a genuine reading becomes a fake one.

pub mod config;
pub mod hook;
pub mod log;
pub mod platform;
pub mod session;
pub mod sync;
pub mod time;

// The updater is a process started beside a session, which is exactly the half of way #2 a build
// without the feature is meant not to carry at all.
#[cfg(feature = "vdso")]
pub mod updater;
