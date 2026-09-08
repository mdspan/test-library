//! The session this process reads, in one word the read path can load.

// Only the Linux arm exports these so far, so on the other platforms nothing calls them yet.
#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "reached only through a platform arm, and only Linux has one"
    )
)]

use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use common::session::layout::Header;

static SESSION: AtomicPtr<Header> = AtomicPtr::new(ptr::null_mut());

/// Publishes the mapped session. Called once, from the constructor, before any interposed call can
/// run in this process.
pub(crate) fn install(header: &'static Header) {
    SESSION.store(ptr::from_ref(header).cast_mut(), Ordering::Release);
}

/// The session, or `None` when this process runs on genuine time.
///
/// One relaxed-cost atomic load and a branch. A fork inherits both the word and the mapping it
/// points at, so a child has nothing to do.
#[inline]
pub(crate) fn session() -> Option<&'static Header> {
    let header = SESSION.load(Ordering::Acquire);
    if header.is_null() {
        return None;
    }
    // Safety: `install` stores a reference that lives for the life of the process, and the mapping
    // it names is shared, so a forked child inherits a pointer that is still valid.
    Some(unsafe { &*header })
}
