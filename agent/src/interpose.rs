//! The bodies behind the interposed names. Portable; the platform arms only export them.
//!
//! Every path here is a read path: no lock, no allocation, nothing written to shared memory, and
//! nothing that could recurse into a symbol we took over.

// Only the Linux arm exports these so far, so on the other platforms nothing calls them yet.
#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "reached only through a platform arm, and only Linux has one"
    )
)]

use common::hook::Func;
use common::platform::{clock, genuine_clock_gettime};
use common::session::clock_now;
use common::time::Nanos;

use crate::state;

/// What the session shows for `clock`, or `None` when the genuine reading is the answer: this
/// process runs on genuine time, or the clock is one the family table leaves alone and no hook took
/// it.
///
/// The hook goes first and the keeper answers what it declines. `func` is the interposed function
/// the program called, which a hook may switch on; every clock read is the same conversion, a
/// genuine reading into a fake one.
#[inline]
fn faked(func: Func, clock: libc::clockid_t) -> Option<Nanos> {
    let keeper = clock_now(state::session()?, clock);
    state::hook().map_or(keeper, |hook| {
        hook.read_clock(func, clock, keeper).or(keeper)
    })
}

/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[inline]
pub(crate) unsafe fn clock_gettime(clock: libc::clockid_t, ts: *mut libc::timespec) -> libc::c_int {
    if !ts.is_null()
        && let Some(now) = faked(Func::ClockGettime, clock)
    {
        // Safety: `ts` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *ts = now.to_timespec() };
        return 0;
    }
    // Safety: `ts` is the caller's, passed on unchanged.
    unsafe { genuine_clock_gettime(clock, ts) }
}

/// # Safety
/// `tv` and `tz` must be null or valid for writes of a `timeval` and a `timezone`.
#[inline]
pub(crate) unsafe fn gettimeofday(tv: *mut libc::timeval, tz: *mut libc::c_void) -> libc::c_int {
    let Some(now) = faked(Func::Gettimeofday, clock::REALTIME) else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { genuine_gettimeofday(tv, tz) };
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

/// # Safety
/// `t` must be null or valid for writes of a `time_t`.
#[inline]
pub(crate) unsafe fn time(t: *mut libc::time_t) -> libc::time_t {
    let seconds = if let Some(now) = faked(Func::Time, clock::REALTIME) {
        now.seconds()
    } else {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        if unsafe { genuine_clock_gettime(clock::REALTIME, &raw mut ts) } != 0 {
            return -1;
        }
        Nanos::from_timespec(&ts).seconds()
    };
    if !t.is_null() {
        // Safety: `t` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *t = seconds };
    }
    seconds
}

/// # Safety
/// `tv` and `tz` must be null or valid for writes.
unsafe fn genuine_gettimeofday(tv: *mut libc::timeval, tz: *mut libc::c_void) -> libc::c_int {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a live, correctly typed buffer.
    if unsafe { genuine_clock_gettime(clock::REALTIME, &raw mut ts) } != 0 {
        return -1;
    }
    if !tv.is_null() {
        // Safety: `tv` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *tv = Nanos::from_timespec(&ts).to_timeval() };
    }
    if !tz.is_null() {
        // Safety: `tz` is non-null and names a `timezone`, which is plain data.
        unsafe { std::ptr::write_bytes(tz.cast::<u8>(), 0, size_of::<libc::timezone>()) };
    }
    0
}
