//! The lease: the lock an updater holds for as long as it is alive.
//!
//! A file of its own beside the session's write lock, because a lock here covers the whole file and
//! sharing one with the write lock would put the updater in the same table as every writer and
//! block them all. The kernel releases it when the holder dies, which is what makes "released"
//! mean "no updater" without anybody having to record a pid.

use std::io;
use std::path::Path;

use crate::platform;
use crate::sync::lock_request;

use super::c_path;

/// How a lease file is opened to be probed: no write access, because the question needs none, and
/// the same refusals of a planted symlink and a planted FIFO the session's own files get.
const PROBE_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

/// How the holder opens it: writable, since an exclusive lock needs a writable descriptor.
const HOLD_FLAGS: libc::c_int =
    libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

/// Whether an updater holds the lease on `path`, asked of the kernel and taking nothing.
///
/// `None` when there was no answer to be had: no file, or a file that is not ours. A caller reads
/// that as an unanswered question rather than as a released lease, because spawning an updater for
/// a session we cannot see is worse than leaving one alone.
///
/// One parser for every platform, the write lock's: any lock kind other than `F_UNLCK` means held.
#[must_use]
pub(super) fn held(path: &Path) -> Option<bool> {
    let c_path = c_path(path).ok()?;
    // Safety: `c_path` is a live NUL-terminated path.
    let fd = unsafe { libc::open(c_path.as_ptr(), PROBE_FLAGS) };
    if fd < 0 {
        return None;
    }
    let mut request = lock_request();
    // Safety: `fd` is open on the lease file and `request` is a live `flock` we just zeroed.
    let answered = unsafe { platform::probe_write_lock(fd, &raw mut request) } == 0;
    // Safety: we opened this descriptor and nothing else holds it.
    unsafe { libc::close(fd) };
    answered.then(|| i64::from(request.l_type) != i64::from(libc::F_UNLCK))
}

/// The lease, held for as long as this value lives, which for an updater is as long as it runs.
#[derive(Debug)]
pub(super) struct Lease {
    fd: libc::c_int,
}

impl Lease {
    /// Takes the lease on `path`, without blocking.
    ///
    /// # Errors
    /// [`io::Error`] when the file cannot be opened, or when another updater already holds it,
    /// which is how two respawners racing settle.
    pub(super) fn take(path: &Path) -> io::Result<Self> {
        let c_path = c_path(path)?;
        // Safety: `c_path` is a live NUL-terminated path.
        let fd = unsafe { libc::open(c_path.as_ptr(), HOLD_FLAGS, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // Safety: `fd` is open on the lease file, for writing.
        if unsafe { platform::take_lease(fd) } != 0 {
            let err = io::Error::last_os_error();
            // Safety: we opened this descriptor and nothing else holds it.
            unsafe { libc::close(fd) };
            return Err(err);
        }
        Ok(Self { fd })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Safety: we own this descriptor. Closing it drops the last reference to the description,
        // and the kernel releases the lease with it.
        unsafe { libc::close(self.fd) };
    }
}
