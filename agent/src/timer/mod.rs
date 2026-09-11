//! Timers: the bodies behind the names the Linux arm exports.
//!
//! A timer makes the same promise a wait makes, and it makes it once, when it is armed: fire when
//! the clock has advanced this far. So an arm converts by the rules [`crate::wait`] converts by. A
//! relative duration is divided by the rate of the family the timer follows, and an absolute
//! expiry becomes a remaining fake duration, then a genuine duration, then a genuine deadline on
//! the timer's own clock. A rate change or a freeze landing afterwards reaches neither a timer nor
//! a sleep, so the two armed together still fire together.
//!
//! Which family a timer follows is the clock the kernel counts it against. `setitimer` follows
//! Realtime for `ITIMER_REAL` and Cpu for the two that count consumption; `timer_settime` and
//! `timerfd_settime` follow the clock the timer was created on, which [`owner`] remembers; `alarm`
//! follows Realtime, being one `ITIMER_REAL` timer under another name.
//!
//! A query is the other direction, and the fourth conversion the hook sees: the kernel counts a
//! timer down in genuine time, and `getitimer`, `timer_gettime` and `timerfd_gettime` owe the
//! program the count in its own units. The `old_value` an arm fills in is the same conversion,
//! because it is the query the arm replaced.
//!
//! Zero is not a duration in either direction. A zero expiry disarms a timer and a zero interval
//! stops it repeating, so neither is scaled; a frozen family must not turn a disarm into the
//! longest duration there is, and a query must not turn a disarmed timer into an armed one.

pub(crate) mod owner;

use std::ptr;

use common::hook::{Func, Kind};
use common::platform::clock;
use common::session::timer_remaining;
use common::time::Nanos;

use crate::wait::{self, MICROS_PER_SECOND, duration_of, unsupported};
use crate::{genuine, state};

/// The clock of the family an interval timer follows, or `None` for a `which` the genuine call
/// would refuse.
///
/// `ITIMER_REAL` counts wall-clock time, and `ITIMER_VIRTUAL` and `ITIMER_PROF` count slices of the
/// process's own consumption, which is the Cpu family.
const fn itimer_clock(which: libc::c_int) -> Option<libc::clockid_t> {
    match which {
        libc::ITIMER_REAL => Some(clock::REALTIME),
        libc::ITIMER_VIRTUAL | libc::ITIMER_PROF => Some(clock::PROCESS_CPUTIME),
        _ => None,
    }
}

/// The genuine `(expiry, interval)` a fake pair becomes, or `None` when the arm passes through as
/// the program wrote it: it disarms the timer, or nothing fakes this clock.
///
/// The expiry is converted first, because it is the field that decides whether the timer is armed
/// at all, and an interval of nothing is a timer that does not repeat rather than a duration.
#[inline]
fn armed(
    func: Func,
    clock: libc::clockid_t,
    expiry: Nanos,
    interval: Nanos,
    absolute: bool,
) -> Option<(Nanos, Nanos)> {
    if expiry.0 == 0 {
        return None;
    }
    let genuine_expiry = if absolute {
        wait::deadline(func, clock, expiry)?
    } else {
        wait::genuine_duration(func, clock, expiry)?
    };
    let genuine_interval = if interval.0 == 0 {
        Nanos::ZERO
    } else {
        wait::genuine_duration(func, clock, interval)?
    };
    Some((genuine_expiry, genuine_interval))
}

/// What a timer's genuine remaining becomes in the program's own units, or `None` when it stands as
/// the kernel left it: nothing is left, this process runs on genuine time, or the clock is one the
/// family table leaves alone and no hook took it.
///
/// The hook goes first, in the genuine-to-fake duration shape, and the keeper answers what it
/// declines.
#[inline]
fn remaining(func: Func, clock: libc::clockid_t, genuine: Nanos) -> Option<Nanos> {
    if genuine.0 == 0 {
        return None;
    }
    let keeper = timer_remaining(state::session()?, clock, genuine);
    state::hook().map_or(keeper, |hook| {
        hook.convert(
            func,
            Kind::DurationToFake,
            clock,
            genuine,
            keeper.unwrap_or(genuine),
        )
        .or(keeper)
    })
}

/// The duration a `timeval` field names, or `None` when the pair is one the genuine call would
/// refuse.
fn interval_of(tv: &libc::timeval) -> Option<Nanos> {
    (tv.tv_sec >= 0 && (0..MICROS_PER_SECOND).contains(&tv.tv_usec))
        .then(|| Nanos::from_timeval(tv))
}

/// The genuine `itimerval` a fake one becomes, or `None` when the arm passes through as the program
/// wrote it.
///
/// # Safety
/// `new_value` must be null or valid for reads of an `itimerval`.
#[inline]
unsafe fn armed_itimerval(
    func: Func,
    clock: libc::clockid_t,
    new_value: *const libc::itimerval,
) -> Option<libc::itimerval> {
    if new_value.is_null() {
        return None;
    }
    // Safety: `new_value` is non-null, so the caller's guarantee makes it valid for this read.
    let asked = unsafe { &*new_value };
    let expiry = interval_of(&asked.it_value)?;
    let interval = interval_of(&asked.it_interval)?;
    // An interval timer's expiry is always relative: `setitimer` has no absolute form.
    let (expiry, interval) = armed(func, clock, expiry, interval, false)?;
    Some(libc::itimerval {
        it_interval: interval.to_timeval(),
        it_value: expiry.to_timeval(),
    })
}

/// Rewrites the remaining the kernel filled in, in genuine units, as the remaining the program is
/// owed in its own.
///
/// # Safety
/// `value` must be null or valid for reads and writes of an `itimerval`.
#[inline]
unsafe fn report_itimerval(func: Func, clock: libc::clockid_t, value: *mut libc::itimerval) {
    if value.is_null() {
        return;
    }
    // Safety: `value` is non-null, so the caller's guarantee makes it valid for this access.
    let left = unsafe { &mut *value };
    if let Some(expiry) = remaining(func, clock, Nanos::from_timeval(&left.it_value)) {
        left.it_value = expiry.to_timeval();
    }
    if let Some(interval) = remaining(func, clock, Nanos::from_timeval(&left.it_interval)) {
        left.it_interval = interval.to_timeval();
    }
}

/// One interval timer armed: the conversion, the genuine call, and the remaining the call it
/// replaced reports back.
///
/// `func` is the name the program called, so a hook sees `alarm` for an alarm and `setitimer` for a
/// `setitimer`, though both arm the one `ITIMER_REAL` timer.
///
/// # Safety
/// `new_value` must be null or valid for reads of an `itimerval`, and `old_value` null or valid for
/// reads and writes of one.
#[inline]
unsafe fn set_interval(
    func: Func,
    which: libc::c_int,
    clock: libc::clockid_t,
    new_value: *const libc::itimerval,
    old_value: *mut libc::itimerval,
) -> libc::c_int {
    // Safety: `new_value` is the caller's, read only where it is non-null.
    let converted = unsafe { armed_itimerval(func, clock, new_value) };
    let request = converted.as_ref().map_or(new_value, ptr::from_ref);
    // Safety: the request is ours or the caller's, and `old_value` passes on unchanged.
    let Some(result) = (unsafe { genuine::setitimer(which, request, old_value) }) else {
        return unsupported();
    };
    if result == 0 {
        // Safety: `old_value` is the caller's, written only where it is non-null.
        unsafe { report_itimerval(func, clock, old_value) };
    }
    result
}

/// # Safety
/// `new_value` must be null or valid for reads of an `itimerval`, and `old_value` null or valid for
/// reads and writes of one.
#[inline]
pub(crate) unsafe fn setitimer(
    which: libc::c_int,
    new_value: *const libc::itimerval,
    old_value: *mut libc::itimerval,
) -> libc::c_int {
    let Some(clock) = itimer_clock(which) else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { genuine::setitimer(which, new_value, old_value) }
            .unwrap_or_else(unsupported);
    };
    // Safety: both pointers are the caller's, passed on under the same contract.
    unsafe { set_interval(Func::Setitimer, which, clock, new_value, old_value) }
}

/// # Safety
/// `curr_value` must be null or valid for reads and writes of an `itimerval`.
#[inline]
pub(crate) unsafe fn getitimer(
    which: libc::c_int,
    curr_value: *mut libc::itimerval,
) -> libc::c_int {
    // Safety: the buffer is the caller's, passed on unchanged.
    let Some(result) = (unsafe { genuine::getitimer(which, curr_value) }) else {
        return unsupported();
    };
    if let (0, Some(clock)) = (result, itimer_clock(which)) {
        // Safety: `curr_value` is the caller's, written only where it is non-null.
        unsafe { report_itimerval(Func::Getitimer, clock, curr_value) };
    }
    result
}

/// Whole seconds of wall-clock alarm, which is the one `ITIMER_REAL` timer under another name, as
/// it is in libc.
///
/// Arming through the interval timer is what keeps the precision a fast clock needs: a fake second
/// under a ten-times clock is a tenth of a genuine one, which no whole count of genuine seconds
/// could name. The answer is the seconds the alarm it replaced still had, rounded up, because a
/// fraction of a second left is a second the program has not had, and zero is what says there was
/// no alarm at all.
#[inline]
pub(crate) fn alarm(seconds: libc::c_uint) -> libc::c_uint {
    let request = libc::itimerval {
        it_interval: Nanos::ZERO.to_timeval(),
        it_value: Nanos(i64::from(seconds).saturating_mul(Nanos::SECOND)).to_timeval(),
    };
    let mut previous = libc::itimerval {
        it_interval: Nanos::ZERO.to_timeval(),
        it_value: Nanos::ZERO.to_timeval(),
    };
    // Safety: both structs are our own live buffers.
    let result = unsafe {
        set_interval(
            Func::Alarm,
            libc::ITIMER_REAL,
            clock::REALTIME,
            &raw const request,
            &raw mut previous,
        )
    };
    if result != 0 {
        return 0;
    }
    let left = Nanos::from_timeval(&previous.it_value)
        .0
        .saturating_add(Nanos::SECOND - 1)
        / Nanos::SECOND;
    libc::c_uint::try_from(left).unwrap_or(libc::c_uint::MAX)
}

/// Learns which clock the timer this creates will arm against, and reads no time.
///
/// # Safety
/// `sevp` must be null or valid for reads of a `sigevent`, and `timerid` valid for writes of a
/// `timer_t`.
#[inline]
pub(crate) unsafe fn timer_create(
    clock: libc::clockid_t,
    sevp: *mut libc::sigevent,
    timerid: *mut libc::timer_t,
) -> libc::c_int {
    // Safety: both pointers are the caller's, passed on unchanged.
    let Some(result) = (unsafe { genuine::timer_create(clock, sevp, timerid) }) else {
        return unsupported();
    };
    if result == 0 && !timerid.is_null() {
        // Safety: the genuine call succeeded, so it filled the caller's slot with the id it made.
        owner::remember_timer(unsafe { *timerid }, clock);
    }
    result
}

/// The genuine `itimerspec` a fake one becomes, or `None` when the arm passes through as the
/// program wrote it.
///
/// # Safety
/// `new_value` must be null or valid for reads of an `itimerspec`.
#[inline]
unsafe fn armed_itimerspec(
    func: Func,
    clock: libc::clockid_t,
    new_value: *const libc::itimerspec,
    absolute: bool,
) -> Option<libc::itimerspec> {
    if new_value.is_null() {
        return None;
    }
    // Safety: `new_value` is non-null, so the caller's guarantee makes it valid for this read.
    let asked = unsafe { &*new_value };
    // The kernel refuses the same pairs for an absolute expiry as for a relative one, so an
    // absolute expiry is read by the same rule and used as the instant it is.
    let expiry = duration_of(&asked.it_value)?;
    let interval = duration_of(&asked.it_interval)?;
    let (expiry, interval) = armed(func, clock, expiry, interval, absolute)?;
    Some(libc::itimerspec {
        it_interval: interval.to_timespec(),
        it_value: expiry.to_timespec(),
    })
}

/// Rewrites a `timespec` pair of remaining, as [`report_itimerval`] rewrites a `timeval` one.
///
/// # Safety
/// `value` must be null or valid for reads and writes of an `itimerspec`.
#[inline]
unsafe fn report_itimerspec(func: Func, clock: libc::clockid_t, value: *mut libc::itimerspec) {
    if value.is_null() {
        return;
    }
    // Safety: `value` is non-null, so the caller's guarantee makes it valid for this access.
    let left = unsafe { &mut *value };
    if let Some(expiry) = remaining(func, clock, Nanos::from_timespec(&left.it_value)) {
        left.it_value = expiry.to_timespec();
    }
    if let Some(interval) = remaining(func, clock, Nanos::from_timespec(&left.it_interval)) {
        left.it_interval = interval.to_timespec();
    }
}

/// # Safety
/// `new_value` must be null or valid for reads of an `itimerspec`, and `old_value` null or valid
/// for reads and writes of one.
#[inline]
pub(crate) unsafe fn timer_settime(
    timerid: libc::timer_t,
    flags: libc::c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> libc::c_int {
    let clock = owner::timer_clock(timerid);
    let absolute = flags & libc::TIMER_ABSTIME != 0;
    // Safety: `new_value` is the caller's, read only where it is non-null.
    let converted = unsafe { armed_itimerspec(Func::TimerSettime, clock, new_value, absolute) };
    let request = converted.as_ref().map_or(new_value, ptr::from_ref);
    // Safety: the request is ours or the caller's, and the timer and `old_value` pass on unchanged.
    let Some(result) = (unsafe { genuine::timer_settime(timerid, flags, request, old_value) })
    else {
        return unsupported();
    };
    if result == 0 {
        // Safety: `old_value` is the caller's, written only where it is non-null.
        unsafe { report_itimerspec(Func::TimerSettime, clock, old_value) };
    }
    result
}

/// # Safety
/// `curr_value` must be null or valid for reads and writes of an `itimerspec`.
#[inline]
pub(crate) unsafe fn timer_gettime(
    timerid: libc::timer_t,
    curr_value: *mut libc::itimerspec,
) -> libc::c_int {
    // Safety: the timer and the buffer are the caller's, passed on unchanged.
    let Some(result) = (unsafe { genuine::timer_gettime(timerid, curr_value) }) else {
        return unsupported();
    };
    if result == 0 {
        // Safety: `curr_value` is the caller's, written only where it is non-null.
        unsafe { report_itimerspec(Func::TimerGettime, owner::timer_clock(timerid), curr_value) };
    }
    result
}

/// Learns which clock the timerfd this creates will arm against, and reads no time.
#[inline]
pub(crate) fn timerfd_create(clock: libc::clockid_t, flags: libc::c_int) -> libc::c_int {
    // Safety: the call takes two plain values and touches no memory of the caller's.
    let Some(fd) = (unsafe { genuine::timerfd_create(clock, flags) }) else {
        return unsupported();
    };
    if fd >= 0 {
        owner::remember_timerfd(fd, clock);
    }
    fd
}

/// # Safety
/// `new_value` must be null or valid for reads of an `itimerspec`, and `old_value` null or valid
/// for reads and writes of one.
#[inline]
pub(crate) unsafe fn timerfd_settime(
    fd: libc::c_int,
    flags: libc::c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> libc::c_int {
    let clock = owner::timerfd_clock(fd);
    let absolute = flags & libc::TFD_TIMER_ABSTIME != 0;
    // Safety: `new_value` is the caller's, read only where it is non-null.
    let converted = unsafe { armed_itimerspec(Func::TimerfdSettime, clock, new_value, absolute) };
    let request = converted.as_ref().map_or(new_value, ptr::from_ref);
    // Safety: the request is ours or the caller's, and `old_value` passes on unchanged.
    let Some(result) = (unsafe { genuine::timerfd_settime(fd, flags, request, old_value) }) else {
        return unsupported();
    };
    if result == 0 {
        // Safety: `old_value` is the caller's, written only where it is non-null.
        unsafe { report_itimerspec(Func::TimerfdSettime, clock, old_value) };
    }
    result
}

/// # Safety
/// `curr_value` must be null or valid for reads and writes of an `itimerspec`.
#[inline]
pub(crate) unsafe fn timerfd_gettime(
    fd: libc::c_int,
    curr_value: *mut libc::itimerspec,
) -> libc::c_int {
    // Safety: the buffer is the caller's, passed on unchanged.
    let Some(result) = (unsafe { genuine::timerfd_gettime(fd, curr_value) }) else {
        return unsupported();
    };
    if result == 0 {
        // Safety: `curr_value` is the caller's, written only where it is non-null.
        unsafe { report_itimerspec(Func::TimerfdGettime, owner::timerfd_clock(fd), curr_value) };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeval(sec: libc::time_t, usec: libc::suseconds_t) -> libc::timeval {
        libc::timeval {
            tv_sec: sec,
            tv_usec: usec,
        }
    }

    #[test]
    fn an_interval_timer_follows_realtime_and_the_two_cpu_timers_follow_cpu() {
        assert_eq!(itimer_clock(libc::ITIMER_REAL), Some(clock::REALTIME));
        assert_eq!(
            itimer_clock(libc::ITIMER_VIRTUAL),
            Some(clock::PROCESS_CPUTIME)
        );
        assert_eq!(
            itimer_clock(libc::ITIMER_PROF),
            Some(clock::PROCESS_CPUTIME)
        );
        // A timer the kernel has no such id for is left for the genuine call to refuse.
        for which in [-1, 3, 4_242] {
            assert_eq!(itimer_clock(which), None, "{which}");
        }
    }

    #[test]
    fn the_pairs_a_genuine_call_would_refuse_name_no_interval() {
        assert_eq!(
            interval_of(&timeval(1, 500)),
            Some(Nanos(Nanos::SECOND + 500_000))
        );
        assert_eq!(interval_of(&timeval(0, 0)), Some(Nanos::ZERO));
        assert_eq!(interval_of(&timeval(-1, 0)), None);
        assert_eq!(interval_of(&timeval(0, MICROS_PER_SECOND)), None);
        assert_eq!(interval_of(&timeval(0, -1)), None);
    }

    #[test]
    fn a_disarm_is_its_own_conversion() {
        // No session is installed in this process, so an arm that got as far as the keeper would
        // answer `None` too. What this pins is that a zero expiry never gets that far: the answer
        // is the same whatever the rate, so nothing can turn a disarm into an arm.
        assert_eq!(
            armed(
                Func::Setitimer,
                clock::REALTIME,
                Nanos::ZERO,
                Nanos(Nanos::SECOND),
                false
            ),
            None
        );
    }

    #[test]
    fn a_timer_with_nothing_left_reports_nothing_left() {
        assert_eq!(
            remaining(Func::Getitimer, clock::REALTIME, Nanos::ZERO),
            None
        );
    }
}
