//! FreeBSD. Empty for now; the shape is here so a second platform needs no restructuring.

use std::io;

/// What the header's platform field carries for FreeBSD.
pub const OS_TAG: u32 = 2;

/// The clock ids of the families, by the names the family table gives them.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_MONOTONIC;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_UPTIME;
    /// The wall clock without leap seconds, which FreeBSD does not keep separately.
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

/// The id the kernel gives this thread.
///
/// What names a thread where thread-local storage cannot: a runtime that makes its own threads
/// without libc leaves the thread pointer holding something of its own.
#[must_use]
pub fn thread_id() -> u64 {
    let mut id: libc::c_long = 0;
    // Safety: `thr_self` writes this thread's own id into a live word and reads nothing.
    if unsafe { libc::thr_self(&raw mut id) } != 0 {
        return 0;
    }
    u64::try_from(id).unwrap_or(0)
}

/// Reads a genuine clock, once this arm exists.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
pub unsafe fn genuine_clock_gettime(
    _clock: libc::clockid_t,
    _ts: *mut libc::timespec,
) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Reads the genuine process times, once this arm exists.
///
/// # Safety
/// `buf` must be null or valid for writes of a `tms`.
pub unsafe fn genuine_times(_buf: *mut libc::tms) -> libc::clock_t {
    unimplemented!("the FreeBSD seam")
}

/// Reads the genuine resource usage of `who`, once this arm exists.
///
/// # Safety
/// `usage` must be valid for writes of a `rusage`.
pub unsafe fn genuine_getrusage(_who: libc::c_int, _usage: *mut libc::rusage) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Sets the genuine wall clock, once this arm exists.
///
/// # Safety
/// `tv` must be null or valid for reads of a `timeval`, and `tz` null or valid for reads of a
/// `timezone`.
pub unsafe fn genuine_settimeofday(
    _tv: *const libc::timeval,
    _tz: *const libc::c_void,
) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Sets a genuine clock, once this arm exists.
///
/// # Safety
/// `tp` must be null or valid for reads of a `timespec`.
pub unsafe fn genuine_clock_settime(
    _clock: libc::clockid_t,
    _tp: *const libc::timespec,
) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Makes the genuine one-shot offset adjustment, once this arm exists.
///
/// # Safety
/// `delta` must be null or valid for reads of a `timeval`, and `olddelta` null or valid for writes
/// of one.
pub unsafe fn genuine_adjtime(
    _delta: *const libc::timeval,
    _olddelta: *mut libc::timeval,
) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// The widest delta the platform's own `adjtime` takes before it refuses, in seconds.
///
/// `adjtime` is a system call of its own here, taking the `timeval` as it comes, so the bound is
/// the range an instant holds rather than anything libc imposes.
pub const ADJTIME_SECONDS: i64 = i64::MAX / 1_000_000_000;

/// Whether the kernel would let this process set the clock: an effective uid of 0 here, since
/// FreeBSD gates the call on the superuser rather than on a capability.
#[must_use]
pub fn may_set_the_clock() -> bool {
    // Safety: `geteuid` reads process state and cannot fail.
    let euid = unsafe { libc::geteuid() };
    euid == 0
}

/// Sets this thread's `errno`, which is how a refused call reports itself in libc's convention.
pub fn set_errno(value: libc::c_int) {
    // Safety: `__error` answers this thread's own `errno`, taking nothing.
    let location = unsafe { libc::__error() };
    // Safety: the location libc just named is live for as long as this thread is.
    unsafe { *location = value };
}

/// Takes the write lock on `fd`, once this arm exists.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file, opened for writing.
pub unsafe fn take_write_lock(_fd: libc::c_int) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Asks the kernel whether the write lock on `fd` is held, once this arm exists.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file and `request` a live `flock`.
pub unsafe fn probe_write_lock(_fd: libc::c_int, _request: *mut libc::flock) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Fills `buf` with randomness, once this arm exists.
///
/// # Errors
/// Whatever the system refused with.
pub fn random_bytes(_buf: &mut [u8]) -> io::Result<()> {
    unimplemented!("the FreeBSD seam")
}
