//! Sleeps and timeouts: the bodies behind the names every platform arm exports.
//!
//! A wait is a promise about faketime: wake when the clock has advanced this far. So every wait
//! converts once, at the call. A relative duration is divided by the rate of the family the wait
//! follows. An absolute deadline becomes a remaining fake duration, then a genuine duration, then
//! a genuine deadline on the wait's own clock. A rate change or a freeze landing afterwards
//! reaches neither the wait nor what it reports when it is cut short, because nothing re-plans a
//! wait already under way.
//!
//! Which family a wait follows is the clock the kernel or libc would have measured it against:
//! `nanosleep`, `poll`, `select`, `epoll` and `kevent` are measured against the monotonic clock,
//! `clock_nanosleep` and the `clockwait` calls against the clock they were handed, a condition
//! variable against the clock it was built with, and everything with an absolute `CLOCK_REALTIME`
//! deadline against the wall clock. The monotonic clock is asked for rather than spelled, because
//! FreeBSD moved the name from one family to the other at 15.0.
//!
//! `epoll` and the two `clockwait` calls are Linux's alone, and `kevent` is the BSDs', in three
//! spellings on macOS, so each carries a `cfg` of its own. So do `thrd_sleep`, `cnd_timedwait`,
//! `mtx_timedlock` and `recvmmsg`: FreeBSD's libc builds each of them out of one call to a wait we
//! already take over, and the arm there leaves the wait to that call rather than converting it
//! twice.
//!
//! macOS has fewer of these than either: no `clock_nanosleep`, no `ppoll`, no timed mutex, rwlock,
//! semaphore, thread join, signal or message queue, and no way to build a condition variable on a
//! clock other than the wall clock. What it has beyond them is the two wider spellings of
//! `kevent`.
//!
//! Every path here converts and then blocks in the genuine function. Nothing is written to shared
//! memory, nothing is allocated, and nothing recurses into a symbol we took over.
//!
//! A timer converts by the same rules when it is armed, so [`crate::timer`] reaches these
//! conversions through [`genuine_duration`] and [`deadline`] rather than building its own.

// Which clock a condition variable was built with, which is a question only where a platform
// lets one be chosen. macOS reads every deadline of one on the wall clock.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) mod cond;

use std::ptr;

use common::hook::{Func, Kind};
use common::platform::{self, clock, monotonic_timebase};
use common::session::wait_duration;
use common::time::Nanos;

use crate::{genuine, interpose, state};

/// Milliseconds as nanoseconds, the unit a `poll` or `epoll` timeout arrives in.
const NANOS_PER_MILLI: i64 = 1_000_000;

/// Microseconds as nanoseconds, the unit `usleep` and a `timeval` count their fractions in.
const NANOS_PER_MICRO: i64 = 1_000;

/// The first microsecond a `timeval`'s fraction may not name, in the width the field has.
pub(crate) const MICROS_PER_SECOND: libc::suseconds_t = 1_000_000;

/// One wait's conversion: the fake duration the program asked for, and the genuine duration it
/// blocks in.
#[derive(Debug, Clone, Copy)]
struct Wait {
    /// What the program asked to wait, in the family's own time.
    asked: Nanos,
    /// What that is on the genuine clock.
    genuine: Nanos,
}

impl Wait {
    /// How much of the fake duration is left when the genuine wait was cut short, in the
    /// proportion this conversion set.
    ///
    /// The conversion the call made stands, so a rate change since then reaches neither the wait
    /// nor the remainder it reports, and a program that sleeps the remainder out waits exactly the
    /// rest of what it asked for.
    fn remaining(self, genuine_left: Nanos) -> Nanos {
        if self.genuine.0 <= 0 {
            return Nanos::ZERO;
        }
        let left = i128::from(genuine_left.0.max(0)) * i128::from(self.asked.0)
            / i128::from(self.genuine.0);
        Nanos(i64::try_from(left).unwrap_or(i64::MAX))
    }
}

/// The genuine duration a fake one becomes, or `None` when the wait belongs to the genuine clock:
/// this process runs on genuine time, or the clock is one the family table leaves alone and no
/// hook took it.
///
/// The hook goes first, in the fake-to-genuine duration shape, and the keeper answers what it
/// declines.
#[inline]
fn converted(func: Func, clock: libc::clockid_t, fake: Nanos) -> Option<Wait> {
    let keeper = wait_duration(state::session()?, clock, fake);
    let genuine = state::hook().map_or(keeper, |hook| {
        hook.convert(
            func,
            Kind::DurationToGenuine,
            clock,
            fake,
            keeper.unwrap_or(fake),
        )
        .or(keeper)
    })?;
    Some(Wait {
        asked: fake,
        genuine,
    })
}

/// The genuine duration a fake one becomes, or `None` when it passes through as the program wrote
/// it, under the rules [`converted`] applies. What a timer arm needs, which carries no remainder.
#[inline]
pub(crate) fn genuine_duration(func: Func, clock: libc::clockid_t, fake: Nanos) -> Option<Nanos> {
    Some(converted(func, clock, fake)?.genuine)
}

/// The genuine deadline on `clock` that a fake deadline becomes, or `None` when the wait passes
/// through as the program wrote it.
///
/// The fake clock first, so the deadline becomes a duration the hook and the keeper can convert,
/// and the genuine clock last, so the deadline it is added to is no older than the reading the
/// duration was measured from. A wait that lands a hair late keeps its promise; one that lands
/// early breaks it.
#[inline]
pub(crate) fn deadline(func: Func, clock: libc::clockid_t, at: Nanos) -> Option<Nanos> {
    let showing = interpose::reading(func, clock)?;
    let remaining = Nanos(at.saturating_sub(showing).0.max(0));
    let wait = converted(func, clock, remaining)?;
    let genuine_now = platform::genuine_nanos(clock)?;
    Some(genuine_now.saturating_add(wait.genuine))
}

/// The instant a deadline's `timespec` names, or `None` when the pair is one the genuine function
/// would refuse. A deadline already past converts to a wait of nothing, which is what the genuine
/// function makes of one too.
fn instant_of(ts: &libc::timespec) -> Option<Nanos> {
    (0..Nanos::SECOND)
        .contains(&ts.tv_nsec)
        .then(|| Nanos::from_timespec(ts))
}

/// The duration a relative `timespec` names, or `None` when the pair is one the genuine function
/// would refuse.
pub(crate) fn duration_of(ts: &libc::timespec) -> Option<Nanos> {
    (ts.tv_sec >= 0 && (0..Nanos::SECOND).contains(&ts.tv_nsec)).then(|| Nanos::from_timespec(ts))
}

/// The genuine timeout a relative `timespec` timeout becomes, or `None` when the call passes
/// through as the program wrote it: no timeout at all, a pair the genuine function would refuse,
/// or a wait nothing fakes.
///
/// # Safety
/// `timeout` must be null or valid for reads of a `timespec`.
#[inline]
unsafe fn relative_timeout(
    func: Func,
    clock: libc::clockid_t,
    timeout: *const libc::timespec,
) -> Option<libc::timespec> {
    if timeout.is_null() {
        return None;
    }
    // Safety: `timeout` is non-null, so the caller's guarantee makes it valid for this read.
    let fake = duration_of(unsafe { &*timeout })?;
    Some(converted(func, clock, fake)?.genuine.to_timespec())
}

/// The genuine deadline an absolute `timespec` deadline becomes, under the same rules.
///
/// # Safety
/// `at` must be null or valid for reads of a `timespec`.
#[inline]
unsafe fn absolute_deadline(
    func: Func,
    clock: libc::clockid_t,
    at: *const libc::timespec,
) -> Option<libc::timespec> {
    if at.is_null() {
        return None;
    }
    // Safety: `at` is non-null, so the caller's guarantee makes it valid for this read.
    let asked = instant_of(unsafe { &*at })?;
    Some(deadline(func, clock, asked)?.to_timespec())
}

/// The genuine timeout in milliseconds a fake one becomes, or `None` when the call passes through:
/// a negative timeout waits for an event and no duration at all.
#[inline]
fn relative_millis(
    func: Func,
    clock: libc::clockid_t,
    timeout: libc::c_int,
) -> Option<libc::c_int> {
    if timeout < 0 {
        return None;
    }
    let fake = Nanos(i64::from(timeout).saturating_mul(NANOS_PER_MILLI));
    Some(to_millis(converted(func, clock, fake)?.genuine))
}

/// Nanoseconds as a count of milliseconds, rounded up so a wait never wakes before the clock has
/// advanced as far as it was asked to, and clamped to the widest such a timeout holds.
///
/// A frozen family reaches here with the longest duration there is, and leaves with the longest a
/// `poll` timeout can name.
fn to_millis(genuine: Nanos) -> libc::c_int {
    let millis = genuine.0.saturating_add(NANOS_PER_MILLI - 1) / NANOS_PER_MILLI;
    libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX)
}

/// What a call reports when this libc defines no function by that name, which only a program
/// calling a name its own libc never had can reach. libc's convention.
pub(crate) fn unsupported() -> libc::c_int {
    platform::set_errno(libc::ENOSYS);
    -1
}

/// The same, for `mq_timedreceive`, whose answer is a count of bytes.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn unsupported_size() -> libc::ssize_t {
    libc::ssize_t::try_from(unsupported()).unwrap_or(-1)
}

/// A buffer for the genuine time a wait did not use, kept only where the program asked for it.
///
/// The genuine call fills this one and the program gets the fake duration it stands for, so a
/// sleep cut short by a signal is owed the rest of what it asked for rather than the rest of what
/// it blocked in.
#[derive(Debug)]
struct Unused {
    /// What the genuine call left, in genuine nanoseconds.
    left: libc::timespec,
    /// Whether the program passed a buffer of its own at all.
    wanted: bool,
}

impl Unused {
    /// The buffer for a call whose caller passed `rem`.
    const fn of(rem: *mut libc::timespec) -> Self {
        Self {
            left: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            wanted: !rem.is_null(),
        }
    }

    /// What to hand the genuine call: our own buffer where the program asked for a remainder, and
    /// null where it did not.
    const fn slot(&mut self) -> *mut libc::timespec {
        if self.wanted {
            &raw mut self.left
        } else {
            ptr::null_mut()
        }
    }

    /// Writes the fake duration the genuine remainder stands for into the program's own buffer.
    ///
    /// # Safety
    /// `rem` must be the pointer this was built from.
    unsafe fn report(&self, wait: Wait, rem: *mut libc::timespec) {
        if self.wanted {
            // Safety: `wanted` is `rem` being non-null, and the caller guarantees the pointer.
            unsafe {
                *rem = wait
                    .remaining(Nanos::from_timespec(&self.left))
                    .to_timespec();
            };
        }
    }
}

/// Blocks for a converted duration in the genuine `nanosleep`, and reports any remainder as the
/// fake duration it stands for.
///
/// # Safety
/// `rem` must be null or valid for writes of a `timespec`.
#[inline]
unsafe fn slept(wait: Wait, rem: *mut libc::timespec) -> libc::c_int {
    let request = wait.genuine.to_timespec();
    let mut unused = Unused::of(rem);
    // Safety: `request` is a live `timespec` and the slot is null or our own buffer.
    let Some(result) = (unsafe { genuine::nanosleep(&raw const request, unused.slot()) }) else {
        return unsupported();
    };
    if result != 0 {
        // Safety: `rem` is the pointer `unused` was built from.
        unsafe { unused.report(wait, rem) };
    }
    result
}

/// # Safety
/// `req` must be null or valid for reads of a `timespec`, and `rem` null or valid for writes of
/// one.
#[inline]
pub(crate) unsafe fn nanosleep(
    req: *const libc::timespec,
    rem: *mut libc::timespec,
) -> libc::c_int {
    // Safety: `req` is the caller's, read only where it is non-null.
    let wait = unsafe { relative_wait(Func::Nanosleep, monotonic_timebase(), req) };
    let Some(wait) = wait else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { genuine::nanosleep(req, rem) }.unwrap_or_else(unsupported);
    };
    // Safety: `rem` is the caller's, passed on unchanged.
    unsafe { slept(wait, rem) }
}

/// The conversion behind a relative sleep, or `None` when the call passes through as the program
/// wrote it.
///
/// # Safety
/// `req` must be null or valid for reads of a `timespec`.
#[inline]
unsafe fn relative_wait(
    func: Func,
    clock: libc::clockid_t,
    req: *const libc::timespec,
) -> Option<Wait> {
    if req.is_null() {
        return None;
    }
    // Safety: `req` is non-null, so the caller's guarantee makes it valid for this read.
    let fake = duration_of(unsafe { &*req })?;
    converted(func, clock, fake)
}

/// A sleep on the clock the program named, relative or absolute as the flags say. Unlike every
/// other wait here, the answer is the `errno` itself rather than -1 beside it.
///
/// # Safety
/// `req` must be null or valid for reads of a `timespec`, and `rem` null or valid for writes of
/// one.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn clock_nanosleep(
    clock: libc::clockid_t,
    flags: libc::c_int,
    req: *const libc::timespec,
    rem: *mut libc::timespec,
) -> libc::c_int {
    if flags & libc::TIMER_ABSTIME != 0 {
        // Safety: `req` is the caller's, read only where it is non-null.
        let converted = unsafe { absolute_deadline(Func::ClockNanosleep, clock, req) };
        let until = converted.as_ref().map_or(req, ptr::from_ref);
        // Safety: the deadline is ours or the caller's, and `rem`, which an absolute sleep never
        // fills, passes on unchanged.
        return unsafe { genuine::clock_nanosleep(clock, flags, until, rem) }
            .unwrap_or(libc::ENOSYS);
    }
    // Safety: `req` is the caller's, read only where it is non-null.
    let wait = unsafe { relative_wait(Func::ClockNanosleep, clock, req) };
    let Some(wait) = wait else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { genuine::clock_nanosleep(clock, flags, req, rem) }.unwrap_or(libc::ENOSYS);
    };
    let request = wait.genuine.to_timespec();
    let mut unused = Unused::of(rem);
    // Safety: `request` is a live `timespec` and the slot is null or our own buffer.
    let Some(result) =
        (unsafe { genuine::clock_nanosleep(clock, flags, &raw const request, unused.slot()) })
    else {
        return libc::ENOSYS;
    };
    if result == libc::EINTR {
        // Safety: `rem` is the pointer `unused` was built from.
        unsafe { unused.report(wait, rem) };
    }
    result
}

/// Whole seconds of sleep, answering the whole seconds still owed when a signal cut it short.
#[inline]
pub(crate) fn sleep(seconds: libc::c_uint) -> libc::c_uint {
    let fake = Nanos(i64::from(seconds).saturating_mul(Nanos::SECOND));
    let Some(wait) = converted(Func::Sleep, monotonic_timebase(), fake) else {
        // Safety: the count is the caller's, passed on unchanged.
        return unsafe { genuine::sleep(seconds) }.unwrap_or(seconds);
    };
    let mut left = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `left` is our own live buffer, which is all this sleep needs.
    if unsafe { slept(wait, &raw mut left) } == 0 {
        return 0;
    }
    // Rounded up, because a fraction of a second left is a second the program has not had.
    let owed = Nanos::from_timespec(&left)
        .0
        .saturating_add(Nanos::SECOND - 1)
        / Nanos::SECOND;
    libc::c_uint::try_from(owed).unwrap_or(seconds)
}

/// Microseconds of sleep.
#[inline]
pub(crate) fn usleep(micros: libc::useconds_t) -> libc::c_int {
    let fake = Nanos(i64::from(micros).saturating_mul(NANOS_PER_MICRO));
    let Some(wait) = converted(Func::Usleep, monotonic_timebase(), fake) else {
        // Safety: the count is the caller's, passed on unchanged.
        return unsafe { genuine::usleep(micros) }.unwrap_or_else(unsupported);
    };
    // Safety: this sleep reports no remainder, so it needs no buffer for one.
    unsafe { slept(wait, ptr::null_mut()) }
}

/// C11's own sleep, which answers 0, -1 for a signal, and something lower for a failure.
///
/// # Safety
/// `duration` must be null or valid for reads of a `timespec`, and `remaining` null or valid for
/// writes of one.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn thrd_sleep(
    duration: *const libc::timespec,
    remaining: *mut libc::timespec,
) -> libc::c_int {
    // Safety: `duration` is the caller's, read only where it is non-null.
    let wait = unsafe { relative_wait(Func::ThrdSleep, monotonic_timebase(), duration) };
    let Some(wait) = wait else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { genuine::thrd_sleep(duration, remaining) }.unwrap_or(-2);
    };
    let request = wait.genuine.to_timespec();
    let mut unused = Unused::of(remaining);
    // Safety: `request` is a live `timespec` and the slot is null or our own buffer.
    let Some(result) = (unsafe { genuine::thrd_sleep(&raw const request, unused.slot()) }) else {
        return -2;
    };
    if result == -1 {
        // Safety: `remaining` is the pointer `unused` was built from.
        unsafe { unused.report(wait, remaining) };
    }
    result
}

/// # Safety
/// `fds` must be valid for `nfds` `pollfd`s.
#[inline]
pub(crate) unsafe fn poll(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: libc::c_int,
) -> libc::c_int {
    let timeout = relative_millis(Func::Poll, monotonic_timebase(), timeout).unwrap_or(timeout);
    // Safety: the descriptors are the caller's, passed on unchanged.
    unsafe { genuine::poll(fds, nfds, timeout) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `fds` must be valid for `nfds` `pollfd`s, `timeout` null or valid for reads of a `timespec`,
/// and `sigmask` null or valid for reads of a `sigset_t`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn ppoll(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::Ppoll, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::ppoll(fds, nfds, timeout, sigmask) }.unwrap_or_else(unsupported)
}

/// # Safety
/// The three sets must be null or valid for writes of an `fd_set`, and `timeout` null or valid for
/// reads and writes of a `timeval`.
#[inline]
pub(crate) unsafe fn select(
    nfds: libc::c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
    timeout: *mut libc::timeval,
) -> libc::c_int {
    let asked = if timeout.is_null() {
        None
    } else {
        // Safety: `timeout` is non-null, so the caller's guarantee makes it valid for this read.
        let tv = unsafe { &*timeout };
        (tv.tv_sec >= 0 && (0..MICROS_PER_SECOND).contains(&tv.tv_usec))
            .then(|| Nanos::from_timeval(tv))
            .and_then(|fake| converted(Func::Select, monotonic_timebase(), fake))
    };
    let Some(wait) = asked else {
        // Safety: every argument is the caller's, passed on unchanged.
        return unsafe { genuine::select(nfds, readfds, writefds, exceptfds, timeout) }
            .unwrap_or_else(unsupported);
    };
    let mut request = wait.genuine.to_timeval();
    // Safety: the sets are the caller's and the timeout is our own live buffer.
    let result = unsafe { genuine::select(nfds, readfds, writefds, exceptfds, &raw mut request) }
        .unwrap_or_else(unsupported);
    // Linux leaves the time not waited in the caller's own struct, and ours holds it in genuine
    // time, so the program gets the fake duration that stands for. FreeBSD leaves the struct as it
    // found it, so what goes back there is the duration the program itself wrote.
    // Safety: `timeout` is non-null, so the caller's guarantee makes it valid for this write.
    unsafe { *timeout = wait.remaining(Nanos::from_timeval(&request)).to_timeval() };
    result
}

/// # Safety
/// The three sets must be null or valid for writes of an `fd_set`, `timeout` null or valid for
/// reads of a `timespec`, and `sigmask` null or valid for reads of a `sigset_t`.
#[inline]
pub(crate) unsafe fn pselect(
    nfds: libc::c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::Pselect, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::pselect(nfds, readfds, writefds, exceptfds, timeout, sigmask) }
        .unwrap_or_else(unsupported)
}

/// # Safety
/// `events` must be valid for writes of `maxevents` `epoll_event`s.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn epoll_wait(
    epfd: libc::c_int,
    events: *mut libc::epoll_event,
    maxevents: libc::c_int,
    timeout: libc::c_int,
) -> libc::c_int {
    let timeout =
        relative_millis(Func::EpollWait, monotonic_timebase(), timeout).unwrap_or(timeout);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::epoll_wait(epfd, events, maxevents, timeout) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `events` must be valid for writes of `maxevents` `epoll_event`s, and `sigmask` null or valid
/// for reads of a `sigset_t`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn epoll_pwait(
    epfd: libc::c_int,
    events: *mut libc::epoll_event,
    maxevents: libc::c_int,
    timeout: libc::c_int,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    let timeout =
        relative_millis(Func::EpollPwait, monotonic_timebase(), timeout).unwrap_or(timeout);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::epoll_pwait(epfd, events, maxevents, timeout, sigmask) }
        .unwrap_or_else(unsupported)
}

/// # Safety
/// `events` must be valid for writes of `maxevents` `epoll_event`s, `timeout` null or valid for
/// reads of a `timespec`, and `sigmask` null or valid for reads of a `sigset_t`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn epoll_pwait2(
    epfd: libc::c_int,
    events: *mut libc::epoll_event,
    maxevents: libc::c_int,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::EpollPwait2, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::epoll_pwait2(epfd, events, maxevents, timeout, sigmask) }
        .unwrap_or_else(unsupported)
}

/// The BSDs' own wait on a set of events, whose timeout is a relative `timespec` the kernel
/// measures against the monotonic clock.
///
/// # Safety
/// `changelist` must be valid for reads of `nchanges` `kevent`s, `eventlist` valid for writes of
/// `nevents` of them, and `timeout` null or valid for reads of a `timespec`.
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
#[inline]
pub(crate) unsafe fn kevent(
    kq: libc::c_int,
    changelist: *const libc::kevent,
    nchanges: libc::c_int,
    eventlist: *mut libc::kevent,
    nevents: libc::c_int,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::Kevent, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::kevent(kq, changelist, nchanges, eventlist, nevents, timeout) }
        .unwrap_or_else(unsupported)
}

/// The same wait over the wider event macOS added beside it, whose timeout is the same relative
/// `timespec` on the same clock. `flags` chooses between waiting and polling and names no duration.
///
/// # Safety
/// `changelist` must be valid for reads of `nchanges` `kevent64_s`, `eventlist` valid for writes
/// of `nevents` of them, and `timeout` null or valid for reads of a `timespec`.
#[cfg(target_os = "macos")]
#[inline]
pub(crate) unsafe fn kevent64(
    kq: libc::c_int,
    changelist: *const libc::kevent64_s,
    nchanges: libc::c_int,
    eventlist: *mut libc::kevent64_s,
    nevents: libc::c_int,
    flags: libc::c_uint,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::Kevent64, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: every other argument is the caller's, passed on unchanged.
    unsafe { genuine::kevent64(kq, changelist, nchanges, eventlist, nevents, flags, timeout) }
        .unwrap_or_else(unsupported)
}

/// The form that carries a quality-of-service class, which converts nothing: it takes no
/// deadline at all.
///
/// A caller that wants one arms an `EVFILT_TIMER` in the change list instead, and a change list is
/// the program's own array of a length we do not know, which nothing on this path may copy. So the
/// row exists to catch the call rather than to alter it, and the timers a caller arms through it,
/// libdispatch's among them, run on genuine time.
///
/// # Safety
/// Every argument must satisfy the contract libSystem gives this function.
#[cfg(darwin_kevent_qos)]
#[inline]
pub(crate) unsafe fn kevent_qos(
    kq: libc::c_int,
    changelist: *const std::ffi::c_void,
    nchanges: libc::c_int,
    eventlist: *mut std::ffi::c_void,
    nevents: libc::c_int,
    data_out: *mut std::ffi::c_void,
    data_available: *mut libc::size_t,
    flags: libc::c_uint,
) -> libc::c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    unsafe {
        genuine::kevent_qos(
            kq,
            changelist,
            nchanges,
            eventlist,
            nevents,
            data_out,
            data_available,
            flags,
        )
    }
    .unwrap_or_else(unsupported)
}

/// Learns which clock this condition variable will read its deadlines on, and reads no time.
///
/// # Safety
/// `cond` must be valid for writes of a `pthread_cond_t`, and `attr` null or valid for reads of a
/// `pthread_condattr_t`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn pthread_cond_init(
    cond: *mut libc::pthread_cond_t,
    attr: *const libc::pthread_condattr_t,
) -> libc::c_int {
    // Safety: both pointers are the caller's, passed on unchanged.
    let result = unsafe { genuine::pthread_cond_init(cond, attr) }.unwrap_or(libc::ENOSYS);
    if result == 0 {
        // Safety: `attr` is the caller's, read only where it is non-null.
        let on = unsafe { attribute_clock(attr) };
        cond::remember(cond, on);
    }
    result
}

/// The clock an attribute names, or `CLOCK_REALTIME` when it names none. A libc without the call
/// has no way to ask for another clock either, so the default is the whole answer there.
///
/// # Safety
/// `attr` must be null or valid for reads of a `pthread_condattr_t`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
unsafe fn attribute_clock(attr: *const libc::pthread_condattr_t) -> libc::clockid_t {
    if attr.is_null() {
        return clock::REALTIME;
    }
    let mut named = clock::REALTIME;
    // Safety: `attr` is the caller's and `named` is our own live buffer. This is not a name we
    // interpose, so the call goes straight to libc.
    let read = unsafe { pthread_condattr_getclock(attr, &raw mut named) };
    if read == 0 { named } else { clock::REALTIME }
}

/// Which clock a condition variable built with this attribute reads its deadlines on, as the
/// FreeBSD binding of libc already declares it.
#[cfg(target_os = "freebsd")]
use libc::pthread_condattr_getclock;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// The same on Linux: libc declares the name, and the crate we bind libc through does not.
    fn pthread_condattr_getclock(
        attr: *const libc::pthread_condattr_t,
        clock: *mut libc::clockid_t,
    ) -> libc::c_int;
}

/// Forgets the variable's clock, and reads no time.
///
/// # Safety
/// `cond` must be valid for writes of a `pthread_cond_t`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn pthread_cond_destroy(cond: *mut libc::pthread_cond_t) -> libc::c_int {
    cond::forget(cond);
    // Safety: the variable is the caller's, passed on unchanged.
    unsafe { genuine::pthread_cond_destroy(cond) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// The variable and the mutex must be the caller's live ones, and `abstime` null or valid for
/// reads of a `timespec`.
#[inline]
pub(crate) unsafe fn pthread_cond_timedwait(
    cond: *mut libc::pthread_cond_t,
    mutex: *mut libc::pthread_mutex_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // The clock the variable was built with where one could be chosen, and the wall clock where
    // the platform reads every such deadline on it and has no attribute to say otherwise.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let on = cond::clock_of(cond);
    #[cfg(target_os = "macos")]
    let on = clock::REALTIME;
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::PthreadCondTimedwait, on, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the variable and the mutex are the caller's, passed on unchanged.
    unsafe { genuine::pthread_cond_timedwait(cond, mutex, abstime) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// As [`pthread_cond_timedwait`].
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn pthread_cond_clockwait(
    cond: *mut libc::pthread_cond_t,
    mutex: *mut libc::pthread_mutex_t,
    clock: libc::clockid_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::PthreadCondClockwait, clock, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the variable and the mutex are the caller's, passed on unchanged.
    unsafe { genuine::pthread_cond_clockwait(cond, mutex, clock, abstime) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// The mutex must be the caller's live one, and `abstime` null or valid for reads of a `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn pthread_mutex_timedlock(
    mutex: *mut libc::pthread_mutex_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted =
        unsafe { absolute_deadline(Func::PthreadMutexTimedlock, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the mutex is the caller's, passed on unchanged.
    unsafe { genuine::pthread_mutex_timedlock(mutex, abstime) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// The lock must be the caller's live one, and `abstime` null or valid for reads of a `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn pthread_rwlock_timedrdlock(
    rwlock: *mut libc::pthread_rwlock_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted =
        unsafe { absolute_deadline(Func::PthreadRwlockTimedrdlock, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the lock is the caller's, passed on unchanged.
    unsafe { genuine::pthread_rwlock_timedrdlock(rwlock, abstime) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// As [`pthread_rwlock_timedrdlock`].
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn pthread_rwlock_timedwrlock(
    rwlock: *mut libc::pthread_rwlock_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted =
        unsafe { absolute_deadline(Func::PthreadRwlockTimedwrlock, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the lock is the caller's, passed on unchanged.
    unsafe { genuine::pthread_rwlock_timedwrlock(rwlock, abstime) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// `retval` must be null or valid for writes of a pointer, and `abstime` null or valid for reads
/// of a `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn pthread_timedjoin_np(
    thread: libc::pthread_t,
    retval: *mut *mut std::ffi::c_void,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted =
        unsafe { absolute_deadline(Func::PthreadTimedjoinNp, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the thread and the slot for its answer are the caller's, passed on unchanged.
    unsafe { genuine::pthread_timedjoin_np(thread, retval, abstime) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// The semaphore must be the caller's live one, and `abstime` null or valid for reads of a
/// `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn sem_timedwait(
    sem: *mut libc::sem_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::SemTimedwait, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the semaphore is the caller's, passed on unchanged.
    unsafe { genuine::sem_timedwait(sem, abstime) }.unwrap_or_else(unsupported)
}

/// # Safety
/// As [`sem_timedwait`].
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn sem_clockwait(
    sem: *mut libc::sem_t,
    clock: libc::clockid_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::SemClockwait, clock, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the semaphore is the caller's, passed on unchanged.
    unsafe { genuine::sem_clockwait(sem, clock, abstime) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `set` must be valid for reads of a `sigset_t`, `info` null or valid for writes of a
/// `siginfo_t`, and `timeout` null or valid for reads of a `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn sigtimedwait(
    set: *const libc::sigset_t,
    info: *mut libc::siginfo_t,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::Sigtimedwait, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: the set and the slot for the signal are the caller's, passed on unchanged.
    unsafe { genuine::sigtimedwait(set, info, timeout) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `list` must be valid for reads of `nent` pointers, and `timeout` null or valid for reads of a
/// `timespec`.
#[inline]
pub(crate) unsafe fn aio_suspend(
    list: *const *const libc::aiocb,
    nent: libc::c_int,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let converted = unsafe { relative_timeout(Func::AioSuspend, monotonic_timebase(), timeout) };
    let timeout = converted.as_ref().map_or(timeout, ptr::from_ref);
    // Safety: the list is the caller's, passed on unchanged.
    unsafe { genuine::aio_suspend(list, nent, timeout) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `msgvec` must be valid for `vlen` `mmsghdr`s, and `timeout` null or valid for reads and writes
/// of a `timespec`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn recvmmsg(
    sockfd: libc::c_int,
    msgvec: *mut libc::mmsghdr,
    vlen: libc::c_uint,
    flags: libc::c_int,
    timeout: *mut libc::timespec,
) -> libc::c_int {
    // Safety: `timeout` is the caller's, read only where it is non-null.
    let wait = unsafe { relative_wait(Func::Recvmmsg, monotonic_timebase(), timeout) };
    let Some(wait) = wait else {
        // Safety: every argument is the caller's, passed on unchanged.
        return unsafe { genuine::recvmmsg(sockfd, msgvec, vlen, flags, timeout) }
            .unwrap_or_else(unsupported);
    };
    let mut request = wait.genuine.to_timespec();
    // Safety: the messages are the caller's and the timeout is our own live buffer.
    let result = unsafe { genuine::recvmmsg(sockfd, msgvec, vlen, flags, &raw mut request) }
        .unwrap_or_else(unsupported);
    // The kernel leaves the time not waited in the struct it was given, so the program gets the
    // fake duration ours holds.
    // Safety: `timeout` is non-null, since a null one took the branch above.
    unsafe { *timeout = wait.remaining(Nanos::from_timespec(&request)).to_timespec() };
    result
}

/// C11's own timed condition wait, whose deadline is always read on `TIME_UTC`, the wall clock.
///
/// # Safety
/// The variable and the mutex must be the caller's live ones, and `deadline` null or valid for
/// reads of a `timespec`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn cnd_timedwait(
    cond: *mut std::ffi::c_void,
    mutex: *mut std::ffi::c_void,
    deadline: *const libc::timespec,
) -> libc::c_int {
    // Safety: `deadline` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::CndTimedwait, clock::REALTIME, deadline) };
    let deadline = converted.as_ref().map_or(deadline, ptr::from_ref);
    // Safety: the variable and the mutex are the caller's, passed on unchanged.
    unsafe { genuine::cnd_timedwait(cond, mutex, deadline) }.unwrap_or(libc::ENOSYS)
}

/// C11's own timed lock, whose deadline is read on the wall clock like `cnd_timedwait`'s.
///
/// # Safety
/// The mutex must be the caller's live one, and `deadline` null or valid for reads of a
/// `timespec`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn mtx_timedlock(
    mutex: *mut std::ffi::c_void,
    deadline: *const libc::timespec,
) -> libc::c_int {
    // Safety: `deadline` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::MtxTimedlock, clock::REALTIME, deadline) };
    let deadline = converted.as_ref().map_or(deadline, ptr::from_ref);
    // Safety: the mutex is the caller's, passed on unchanged.
    unsafe { genuine::mtx_timedlock(mutex, deadline) }.unwrap_or(libc::ENOSYS)
}

/// # Safety
/// `msg` must be valid for reads of `len` bytes, and `abstime` null or valid for reads of a
/// `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn mq_timedsend(
    mqdes: libc::mqd_t,
    msg: *const libc::c_char,
    len: libc::size_t,
    priority: libc::c_uint,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::MqTimedsend, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the queue and the message are the caller's, passed on unchanged.
    unsafe { genuine::mq_timedsend(mqdes, msg, len, priority, abstime) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `msg` must be valid for writes of `len` bytes, `priority` null or valid for writes of an
/// unsigned, and `abstime` null or valid for reads of a `timespec`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub(crate) unsafe fn mq_timedreceive(
    mqdes: libc::mqd_t,
    msg: *mut libc::c_char,
    len: libc::size_t,
    priority: *mut libc::c_uint,
    abstime: *const libc::timespec,
) -> libc::ssize_t {
    // Safety: `abstime` is the caller's, read only where it is non-null.
    let converted = unsafe { absolute_deadline(Func::MqTimedreceive, clock::REALTIME, abstime) };
    let abstime = converted.as_ref().map_or(abstime, ptr::from_ref);
    // Safety: the queue and the buffer are the caller's, passed on unchanged.
    unsafe { genuine::mq_timedreceive(mqdes, msg, len, priority, abstime) }
        .unwrap_or_else(unsupported_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn converting(asked: i64, genuine: i64) -> Wait {
        Wait {
            asked: Nanos(asked),
            genuine: Nanos(genuine),
        }
    }

    #[test]
    fn a_remainder_comes_back_in_the_proportion_the_call_set() {
        // Ten times faster: a second asked for is a tenth of a second genuine, and half of that
        // genuine remainder is half a fake second.
        let wait = converting(Nanos::SECOND, Nanos::SECOND / 10);
        assert_eq!(wait.remaining(Nanos(50_000_000)), Nanos(Nanos::SECOND / 2));
        assert_eq!(wait.remaining(Nanos::ZERO), Nanos::ZERO);
        assert_eq!(
            wait.remaining(Nanos(Nanos::SECOND / 10)),
            Nanos(Nanos::SECOND)
        );

        // Half speed, the other direction.
        let wait = converting(Nanos::SECOND, 2 * Nanos::SECOND);
        assert_eq!(
            wait.remaining(Nanos(Nanos::SECOND)),
            Nanos(Nanos::SECOND / 2)
        );
    }

    #[test]
    fn a_wait_of_nothing_has_nothing_left() {
        assert_eq!(
            converting(Nanos::SECOND, 0).remaining(Nanos(5)),
            Nanos::ZERO
        );
        assert_eq!(converting(0, 0).remaining(Nanos(5)), Nanos::ZERO);
        // A genuine remainder the kernel could not have left is no remainder either.
        assert_eq!(converting(10, 10).remaining(Nanos(-5)), Nanos::ZERO);
    }

    #[test]
    fn a_remainder_saturates_instead_of_wrapping() {
        let frozen = converting(i64::MAX, i64::MAX);
        assert_eq!(frozen.remaining(Nanos(i64::MAX)), Nanos(i64::MAX));
    }

    #[test]
    fn a_millisecond_timeout_rounds_up_and_clamps() {
        assert_eq!(to_millis(Nanos::ZERO), 0);
        assert_eq!(to_millis(Nanos(1)), 1, "a wait of nothing would wake early");
        assert_eq!(to_millis(Nanos(NANOS_PER_MILLI)), 1);
        assert_eq!(to_millis(Nanos(NANOS_PER_MILLI + 1)), 2);
        // A frozen family arrives with the longest duration there is and leaves with the longest
        // such a timeout can name.
        assert_eq!(to_millis(Nanos(i64::MAX)), libc::c_int::MAX);
    }

    #[test]
    fn the_pairs_a_genuine_call_would_refuse_name_no_duration() {
        let pair = |sec, nsec| libc::timespec {
            tv_sec: sec,
            tv_nsec: nsec,
        };
        assert_eq!(duration_of(&pair(1, 500)), Some(Nanos(Nanos::SECOND + 500)));
        assert_eq!(duration_of(&pair(0, 0)), Some(Nanos::ZERO));
        assert_eq!(duration_of(&pair(-1, 0)), None);
        assert_eq!(duration_of(&pair(0, Nanos::SECOND)), None);
        assert_eq!(duration_of(&pair(0, -1)), None);

        // A deadline may sit before the epoch; the wait it names is simply over already.
        assert_eq!(instant_of(&pair(-1, 0)), Some(Nanos(-Nanos::SECOND)));
        assert_eq!(instant_of(&pair(0, Nanos::SECOND)), None);
    }
}
