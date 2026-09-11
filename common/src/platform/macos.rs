//! macOS. Empty for now; the shape is here so a second platform needs no restructuring.

use std::io;

/// What the header's platform field carries for macOS.
pub const OS_TAG: u32 = 3;

/// The clock ids of the families, by the names the family table gives them.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_UPTIME_RAW;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_MONOTONIC;
    /// The wall clock without leap seconds, which macOS does not keep separately.
    pub const TAI: clockid_t = libc::CLOCK_REALTIME;
    /// CPU time consumed by the whole process.
    pub const PROCESS_CPUTIME: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    /// CPU time consumed by the calling thread.
    pub const THREAD_CPUTIME: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    /// Every clock a family holds, by the name a user writes for a timebase. Filled in with this
    /// platform's column of the family table when the arm lands.
    pub const NAMED: &[(&str, clockid_t)] = &[];

    /// The family a clock id belongs to, or `None` when the id is left on genuine time. Nothing is
    /// faked here yet, so every id passes through.
    #[must_use]
    pub const fn family_of(_clock: clockid_t) -> Option<Family> {
        None
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    #[must_use]
    pub const fn is_tai(_clock: clockid_t) -> bool {
        false
    }
}

/// Reads a genuine clock, once this arm exists.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
pub unsafe fn genuine_clock_gettime(
    _clock: libc::clockid_t,
    _ts: *mut libc::timespec,
) -> libc::c_int {
    unimplemented!("the macOS seam")
}

/// Reads the genuine process times, once this arm exists.
///
/// # Safety
/// `buf` must be null or valid for writes of a `tms`.
pub unsafe fn genuine_times(_buf: *mut libc::tms) -> libc::clock_t {
    unimplemented!("the macOS seam")
}

/// Reads the genuine resource usage of `who`, once this arm exists.
///
/// # Safety
/// `usage` must be valid for writes of a `rusage`.
pub unsafe fn genuine_getrusage(_who: libc::c_int, _usage: *mut libc::rusage) -> libc::c_int {
    unimplemented!("the macOS seam")
}

/// Takes the write lock on `fd`, once this arm exists.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file, opened for writing.
pub unsafe fn take_write_lock(_fd: libc::c_int) -> libc::c_int {
    unimplemented!("the macOS seam")
}

/// Asks the kernel whether the write lock on `fd` is held, once this arm exists.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file and `request` a live `flock`.
pub unsafe fn probe_write_lock(_fd: libc::c_int, _request: *mut libc::flock) -> libc::c_int {
    unimplemented!("the macOS seam")
}

/// Fills `buf` with randomness, once this arm exists.
///
/// # Errors
/// Whatever the system refused with.
pub fn random_bytes(_buf: &mut [u8]) -> io::Result<()> {
    unimplemented!("the macOS seam")
}
