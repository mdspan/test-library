//! The clock entries of the synthetic vDSO: the functions a target calls when it finds our object
//! instead of the kernel's.
//!
//! Each one answers in the kernel's convention rather than libc's: zero, or the negative of what
//! the call was refused with, and `errno` untouched. That is what a vDSO's caller expects, and it
//! is also what keeps these safe on a thread the target's own runtime made. Such a thread may have
//! no thread-local storage set up the way libc expects, so an entry that wrote `errno` would write
//! somewhere nobody owns.
//!
//! Everything else is the read path the interposed libc names run: the same hook, the same keeper,
//! the same seqlock read flow. That is what makes a target reading through the vDSO and a library
//! reading through libc agree, in one process, on one clock.

use common::hook::Func;
use common::platform::{clock, kernel_clock_gettime, kernel_gettimeofday};

use crate::interpose;

/// The vDSO's own `clock_gettime`.
///
/// # Safety
/// Called by the target under the contract the kernel's vDSO gives this function: `ts` null or
/// valid for writes of a `timespec`.
pub(super) unsafe extern "C" fn clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_long {
    if !ts.is_null()
        && let Some(now) = interpose::faked(Func::ClockGettime, clock)
    {
        // Safety: `ts` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *ts = now.to_timespec() };
        return 0;
    }
    // Safety: `ts` is the caller's, passed on unchanged.
    unsafe { kernel_clock_gettime(clock, ts) }
}

/// The vDSO's own `gettimeofday`.
///
/// # Safety
/// Called by the target under the contract the kernel's vDSO gives this function: `tv` null or
/// valid for writes of a `timeval`, and `tz` null or valid for writes of a `timezone`.
pub(super) unsafe extern "C" fn gettimeofday(
    tv: *mut libc::timeval,
    tz: *mut libc::c_void,
) -> libc::c_long {
    let Some(now) = interpose::faked(Func::Gettimeofday, clock::REALTIME) else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { kernel_gettimeofday(tv, tz) };
    };
    if !tv.is_null() {
        // Safety: `tv` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *tv = now.to_timeval() };
    }
    // The timezone argument is obsolete and the kernel zeroes it; so do we.
    if !tz.is_null() {
        // Safety: `tz` is non-null and names a `timezone`, which is plain data.
        unsafe { std::ptr::write_bytes(tz.cast::<u8>(), 0, size_of::<libc::timezone>()) };
    }
    0
}

/// The vDSO's own `time`, which answers the seconds rather than a status.
///
/// # Safety
/// Called by the target under the contract the kernel's vDSO gives this function: `t` null or
/// valid for writes of a `time_t`.
pub(super) unsafe extern "C" fn time(t: *mut libc::time_t) -> libc::time_t {
    let seconds = if let Some(now) = interpose::faked(Func::Time, clock::REALTIME) {
        now.seconds()
    } else {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        if unsafe { kernel_clock_gettime(clock::REALTIME, &raw mut ts) } != 0 {
            return -1;
        }
        ts.tv_sec
    };
    if !t.is_null() {
        // Safety: `t` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *t = seconds };
    }
    seconds
}
