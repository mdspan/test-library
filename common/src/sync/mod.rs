//! The seqlock, and the two flows around it.
//!
//! Readers project the clock from the live parameters and validate what they read; a writer opens
//! a section, re-anchors every family inside it, and publishes. A writer killed at any instant
//! leaves a whole copy behind, readers keep completing past it, and the next writer repairs.
//!
//! `R1` through `R10` and `W1` through `W12` name the steps of the two flows, and the memory
//! ordering on every access here is the one that step is owed.

mod lock;
mod read;
mod write;

pub use lock::{WriteLock, create, install, lock_request, probe_held};
pub use read::{READ_PATIENCE_NS, read};
pub use write::{Anchor, WRITE_SETTLE_NS, begin};

#[cfg(test)]
mod tests;
