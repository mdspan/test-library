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

use common::hook::{Func, Kind};
use common::platform::{
    clock, genuine_clock_gettime, genuine_getrusage, genuine_nanos, genuine_times,
};
use common::session::{clock_now, cpu_scaled};
use common::time::Nanos;

use crate::state;

/// The base C11 gives `timespec_get` for the wall clock, and the only one any libc we support
/// defines.
const TIME_UTC: libc::c_int = 1;

/// What `clock` counts its answer in. POSIX fixes it at a million on an XSI system, which every
/// platform we support is.
const CLOCKS_PER_SEC: i64 = 1_000_000;

/// The failure `clock` and `times` report: `(clock_t)-1`, whose sign differs by platform.
const NO_TICKS: libc::clock_t = !0;

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

/// A reading of `clock` as the program should see it: faked where the session fakes it, genuine
/// where it does not. `None` only when the kernel refuses the clock, which every caller reports the
/// way its own function reports a failure.
#[inline]
fn reading(func: Func, clock: libc::clockid_t) -> Option<Nanos> {
    faked(func, clock).or_else(|| genuine_nanos(clock))
}

/// What the session shows for an amount of CPU time the program has already consumed, or `None`
/// when the genuine amount stands.
///
/// The shape `times` and `getrusage` need: the consumption arrives in the call's own fields, so the
/// number to convert comes from there rather than from a clock read. It is still a reading coming
/// towards the program, so it is the same kind as one.
#[inline]
fn faked_cpu(func: Func, clock: libc::clockid_t, genuine: Nanos) -> Option<Nanos> {
    let keeper = cpu_scaled(state::session()?, clock, genuine);
    state::hook().map_or(keeper, |hook| {
        hook.convert(
            func,
            Kind::InstantToFake,
            clock,
            genuine,
            keeper.unwrap_or(genuine),
        )
        .or(keeper)
    })
}

/// A clock read that answers in a `timespec`, under whichever name the program called.
///
/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[inline]
unsafe fn read_timespec(
    func: Func,
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    if !ts.is_null()
        && let Some(now) = faked(func, clock)
    {
        // Safety: `ts` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *ts = now.to_timespec() };
        return 0;
    }
    // Safety: `ts` is the caller's, passed on unchanged.
    unsafe { genuine_clock_gettime(clock, ts) }
}

/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[inline]
pub(crate) unsafe fn clock_gettime(clock: libc::clockid_t, ts: *mut libc::timespec) -> libc::c_int {
    // Safety: `ts` is the caller's, passed on unchanged.
    unsafe { read_timespec(Func::ClockGettime, clock, ts) }
}

/// C11's own wall-clock read, which answers the base it was given rather than zero.
///
/// `TIME_UTC` is the one base a C library defines; any other is a failure there, and here.
///
/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[inline]
pub(crate) unsafe fn timespec_get(ts: *mut libc::timespec, base: libc::c_int) -> libc::c_int {
    if base != TIME_UTC || ts.is_null() {
        return 0;
    }
    // Safety: `ts` is non-null and the caller guarantees it.
    if unsafe { read_timespec(Func::TimespecGet, clock::REALTIME, ts) } == 0 {
        base
    } else {
        0
    }
}

/// # Safety
/// `tv` and `tz` must be null or valid for writes of a `timeval` and a `timezone`.
#[inline]
pub(crate) unsafe fn gettimeofday(tv: *mut libc::timeval, tz: *mut libc::c_void) -> libc::c_int {
    let Some(now) = reading(Func::Gettimeofday, clock::REALTIME) else {
        return -1;
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
    let Some(now) = reading(Func::Time, clock::REALTIME) else {
        return -1;
    };
    let seconds = now.seconds();
    if !t.is_null() {
        // Safety: `t` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *t = seconds };
    }
    seconds
}

/// The buffer `ftime` fills. No libc declares it any more: glibc dropped `<sys/timeb.h>` at 2.33
/// and kept the symbol, so a program built against an older one still calls it.
#[derive(Debug)]
#[repr(C)]
pub(crate) struct Timeb {
    /// Whole seconds of the wall clock.
    time: libc::time_t,
    /// Milliseconds past that second.
    millitm: libc::c_ushort,
    /// Minutes west of UTC, which the kernel does not keep. Zero, as libc left it.
    timezone: libc::c_short,
    /// Whether daylight saving applies, zero for the same reason.
    dstflag: libc::c_short,
}

/// # Safety
/// `tp` must be null or valid for writes of a `timeb`.
#[inline]
pub(crate) unsafe fn ftime(tp: *mut Timeb) -> libc::c_int {
    // Unlike `time`, this has nowhere but the buffer to answer in, so a null one is a failure.
    if tp.is_null() {
        return -1;
    }
    let Some(now) = reading(Func::Ftime, clock::REALTIME) else {
        return -1;
    };
    let millis = now.0.rem_euclid(Nanos::SECOND) / 1_000_000;
    // Safety: `tp` is non-null, so the caller's guarantee makes it valid for this write.
    unsafe {
        *tp = Timeb {
            time: now.seconds(),
            millitm: libc::c_ushort::try_from(millis).unwrap_or(0),
            timezone: 0,
            dstflag: 0,
        }
    };
    0
}

/// The CPU time this process has consumed, in the units `CLOCKS_PER_SEC` names rather than
/// nanoseconds.
#[inline]
pub(crate) fn clock() -> libc::clock_t {
    let Some(cpu) = reading(Func::Clock, clock::PROCESS_CPUTIME) else {
        return NO_TICKS;
    };
    to_ticks(cpu, CLOCKS_PER_SEC)
}

/// The process times: consumption in `buf`, which follows the Cpu family, and elapsed time since an
/// arbitrary point as the answer, which is time passing and so follows Monotonic.
///
/// # Safety
/// `buf` must be null or valid for writes of a `tms`.
#[inline]
pub(crate) unsafe fn times(buf: *mut libc::tms) -> libc::clock_t {
    // The genuine call first: the fields it fills are what there is to scale, and nothing else
    // knows what the children of this process have consumed.
    // Safety: `buf` is the caller's, passed on unchanged.
    let elapsed = unsafe { genuine_times(buf) };
    if state::session().is_none() {
        return elapsed;
    }
    let per_second = ticks_per_second();
    if !buf.is_null() {
        // Safety: `buf` is non-null, so the caller's guarantee makes it valid for this write, and
        // the genuine call above has filled it.
        let tms = unsafe { &mut *buf };
        // The children's two fields are consumption as well, so they scale with the other two.
        for field in [
            &mut tms.tms_utime,
            &mut tms.tms_stime,
            &mut tms.tms_cutime,
            &mut tms.tms_cstime,
        ] {
            *field = scaled_ticks(*field, per_second);
        }
    }
    if elapsed == NO_TICKS {
        return elapsed;
    }
    faked(Func::Times, clock::MONOTONIC).map_or(elapsed, |now| to_ticks(now, per_second))
}

/// The resource usage of `who`, with the user and system times scaled by the Cpu family and every
/// other field left as the kernel gave it.
///
/// # Safety
/// `usage` must be valid for writes of a `rusage`.
#[inline]
pub(crate) unsafe fn getrusage(who: libc::c_int, usage: *mut libc::rusage) -> libc::c_int {
    // Safety: `usage` is the caller's, passed on unchanged.
    let result = unsafe { genuine_getrusage(who, usage) };
    if result != 0 || usage.is_null() || state::session().is_none() {
        return result;
    }
    let clock = rusage_clock(who);
    // Safety: `usage` is non-null, the caller guarantees it, and the genuine call has filled it.
    let usage = unsafe { &mut *usage };
    for field in [&mut usage.ru_utime, &mut usage.ru_stime] {
        let genuine = Nanos::from_timeval(field);
        if let Some(faked) = faked_cpu(Func::Getrusage, clock, genuine) {
            *field = faked.to_timeval();
        }
    }
    0
}

/// Which Cpu clock a `getrusage` target names: the whole process for itself and its children, the
/// calling thread for anything else, which is the target a platform that has one spells this way.
const fn rusage_clock(who: libc::c_int) -> libc::clockid_t {
    if who == libc::RUSAGE_SELF || who == libc::RUSAGE_CHILDREN {
        clock::PROCESS_CPUTIME
    } else {
        clock::THREAD_CPUTIME
    }
}

/// One field of a `tms`, scaled by the Cpu family: ticks out to nanoseconds, through the hook and
/// the keeper, and back to ticks.
#[inline]
fn scaled_ticks(ticks: libc::clock_t, per_second: i64) -> libc::clock_t {
    let genuine = from_ticks(ticks, per_second);
    faked_cpu(Func::Times, clock::PROCESS_CPUTIME, genuine)
        .map_or(ticks, |faked| to_ticks(faked, per_second))
}

/// How many ticks a second holds, in the units `times` counts in.
///
/// From the auxiliary vector by way of libc, which neither allocates nor locks to answer it.
fn ticks_per_second() -> i64 {
    // Safety: `sysconf` takes a name and answers a number, touching nothing of ours.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks } else { 100 }
}

/// Nanoseconds as a count of ticks, truncated towards zero the way a tick count always is.
#[inline]
fn to_ticks(value: Nanos, per_second: i64) -> libc::clock_t {
    let ticks = i128::from(value.0) * i128::from(per_second) / i128::from(Nanos::SECOND);
    libc::clock_t::try_from(ticks).unwrap_or(NO_TICKS)
}

/// A count of ticks as nanoseconds.
#[inline]
fn from_ticks(ticks: libc::clock_t, per_second: i64) -> Nanos {
    let nanos = i128::from(ticks) * i128::from(Nanos::SECOND) / i128::from(per_second.max(1));
    Nanos(i64::try_from(nanos).unwrap_or(i64::MAX))
}
