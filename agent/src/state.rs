//! The session this process reads and the hook in front of its keeper, in two words the read
//! path can load.

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

use common::hook::Hook;
use common::session::layout::Header;

static SESSION: AtomicPtr<Header> = AtomicPtr::new(ptr::null_mut());
static HOOK: AtomicPtr<Hook> = AtomicPtr::new(ptr::null_mut());

/// Publishes the mapped session and the hook in front of its keeper. Called once, from the
/// constructor, before any interposed call can run in this process.
///
/// The hook lands first, so a thread that sees the session sees the whole of it: a read either finds
/// no session and runs genuine, or finds the session with its hook already in place.
pub(crate) fn install(header: &'static Header, hook: Option<&'static Hook>) {
    if let Some(hook) = hook {
        HOOK.store(ptr::from_ref(hook).cast_mut(), Ordering::Release);
    }
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

/// The hook in front of the keeper, or `None` when the session has none.
///
/// One atomic load and a branch, like [`session`]. A process that runs on genuine time never gets
/// here, because the session word is null and the read path stops there.
#[inline]
pub(crate) fn hook() -> Option<&'static Hook> {
    let hook = HOOK.load(Ordering::Acquire);
    if hook.is_null() {
        return None;
    }
    // Safety: `install` stores a reference that lives for the life of the process, and a forked
    // child inherits both the word and the library it names.
    Some(unsafe { &*hook })
}
