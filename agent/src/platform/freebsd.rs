//! FreeBSD: `LD_PRELOAD` puts us ahead of libc, so defining the name is the whole mechanism, as
//! on Linux.
//!
//! The clock group is here. `ftime` is not among it: FreeBSD's libc dropped the function long ago,
//! so there is no call of the program's to catch. The sleeps, the timeouts, the timers, the file
//! timestamps and the setters land here later, and so does the timekeep page that catches Go.

use crate::interpose;

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::clock_gettime(clock, ts) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn gettimeofday(
    tv: *mut libc::timeval,
    tz: *mut libc::c_void,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::gettimeofday(tv, tz) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn time(t: *mut libc::time_t) -> libc::time_t {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::time(t) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timespec_get(
    ts: *mut libc::timespec,
    base: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::timespec_get(ts, base) }
}

/// The CPU time this process has consumed, which takes no argument and so needs no contract.
#[unsafe(no_mangle)]
pub(super) extern "C" fn clock() -> libc::clock_t {
    interpose::clock()
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn times(buf: *mut libc::tms) -> libc::clock_t {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::times(buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn getrusage(
    who: libc::c_int,
    usage: *mut libc::rusage,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::getrusage(who, usage) }
}
