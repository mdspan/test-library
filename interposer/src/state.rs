//! The session this process reads, the hook in front of its keeper, and the lock file a setter
//! takes the write lock on.

use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

use common::hook::Hook;
use common::session::layout::Header;

static SESSION: AtomicPtr<Header> = AtomicPtr::new(ptr::null_mut());
static HOOK: AtomicPtr<Hook> = AtomicPtr::new(ptr::null_mut());
static LOCK_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Publishes the mapped session, the hook in front of its keeper, and the lock file of the
/// session. Called once, from the constructor, before any interposed call can run in this process.
///
/// The session word lands last, so a thread that sees the session sees the whole of it: a call
/// either finds no session and runs genuine, or finds the session with its hook and its lock file
/// already in place.
pub(crate) fn install(header: &'static Header, hook: Option<&'static Hook>, lock_file: PathBuf) {
    drop(LOCK_FILE.set(lock_file));
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

/// The lock file a setter takes the write lock on, or `None` when this process runs on genuine
/// time.
///
/// A path rather than a descriptor, because the lock belongs to the open file description one
/// `open` call creates: a writer opens its own, and a descriptor held for the life of the process
/// would be a number the program can see.
pub(crate) fn lock_file() -> Option<&'static Path> {
    LOCK_FILE.get().map(PathBuf::as_path)
}
