//! Linux: `LD_PRELOAD` puts us ahead of libc, so defining the name is the whole mechanism.
//!
//! The rest of the clock group, and the synthetic vDSO that catches Go, land here later.

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
