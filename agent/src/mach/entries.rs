//! The bodies behind the Mach rows, and the timebase they convert through.
//!
//! Which family a counter belongs to is whether it stops while the machine sleeps.
//! `mach_absolute_time` and `mach_approximate_time` stop, so they follow Monotonic;
//! `mach_continuous_time` and `mach_continuous_approximate_time` count on, so they follow
//! Boottime. Each names the `clock_gettime` id libc builds out of that very call:
//! `CLOCK_UPTIME_RAW` and `CLOCK_UPTIME_RAW_APPROX` for the first pair, `CLOCK_MONOTONIC_RAW` and
//! `CLOCK_MONOTONIC_RAW_APPROX` for the second. Each answers the family's own nanoseconds turned
//! back into ticks, and every clock of a family reads the family's value, so a Mach call and the
//! `clock_gettime` beside it agree by construction.
//!
//! `clock_gettime_nsec_np` takes those same ids and follows whichever family each is in.
//!
//! A clock port is a name rather than a clock: `host_get_clock_service` hands one out and
//! `clock_get_time` reads it, so the identity is learned where it is chosen and looked up again at
//! the read. `SYSTEM_CLOCK` counts from boot without suspend, so it is Monotonic's, and
//! `CALENDAR_CLOCK` is the wall clock. `REALTIME_CLOCK` is `SYSTEM_CLOCK` under a second name and
//! the same number, whichever of the two a program writes.
//!
//! Every path here is the read path the rest of the library keeps: no lock, no allocation, nothing
//! written to shared memory, and nothing that could recurse into a symbol we took over.

use std::sync::atomic::{AtomicU64, Ordering};

use common::hook::Func;
use common::platform::clock;
use common::time::Nanos;

use super::Ratio;
use crate::{clocks, genuine, interpose, wait};

/// What a Mach routine answers when it worked.
const KERN_SUCCESS: libc::kern_return_t = 0;

/// The general refusal, which is what a routine libSystem turns out not to have comes back as.
const KERN_FAILURE: libc::kern_return_t = 5;

/// The Mach clock that counts from boot and stops while the machine sleeps, which is Monotonic's.
/// `REALTIME_CLOCK` is this same number, whatever its name suggests.
const SYSTEM_CLOCK: libc::c_int = 0;

/// The Mach clock that is the wall clock.
const CALENDAR_CLOCK: libc::c_int = 1;

/// What `mach_timebase_info` fills.
///
/// Spelled here rather than taken from libc's bindings, which mark every Mach name of theirs
/// deprecated in favour of a crate this workspace does not depend on.
#[derive(Debug)]
#[repr(C)]
struct TimebaseInfo {
    /// Nanoseconds per `denom` ticks.
    numer: u32,
    /// Ticks per `numer` nanoseconds.
    denom: u32,
}

unsafe extern "C" {
    /// The ratio a program converts a count of ticks with. Not a name we stand in front of: faking
    /// it would leave the program converting our ticks with a ratio the counter never ran at.
    fn mach_timebase_info(info: *mut TimebaseInfo) -> libc::kern_return_t;
}

/// The buffer `clock_get_time` fills: Mach's own pair, narrower than a `timespec`.
///
/// The seconds are unsigned and 32 bits wide, so a reading this carries cannot name an instant
/// before 1970 or after 2106 whatever the session shows. A faked clock outside that range is
/// clamped into it here, which is the same wall this call has without us.
#[derive(Debug)]
#[repr(C)]
pub(crate) struct MachTimespec {
    /// Whole seconds.
    tv_sec: libc::c_uint,
    /// Nanoseconds past that second.
    tv_nsec: libc::c_int,
}

impl MachTimespec {
    /// The instant `now` in the pair this carries.
    fn of(now: Nanos) -> Self {
        let nanos = now.0.rem_euclid(Nanos::SECOND);
        Self {
            tv_sec: u32::try_from(now.seconds().clamp(0, i64::from(u32::MAX))).unwrap_or(0),
            tv_nsec: libc::c_int::try_from(nanos).unwrap_or(0),
        }
    }
}

/// The ratio packed into one word, or zero before anyone has asked for it. No pair the kernel
/// answers packs to zero, since neither half of one is.
static TIMEBASE: AtomicU64 = AtomicU64::new(0);

/// Which clock each clock port names, keyed by the port.
///
/// Nothing forgets a port. A program that deallocates one and is handed the same name back for
/// something else would have to call `clock_get_time` on that something else to notice, which is a
/// call the kernel refuses with or without us.
static PORTS: clocks::Table = clocks::Table::new();

/// The ratio this machine's counter runs at.
///
/// Relaxed both ways: the kernel fixes the pair before the first process of the boot runs and
/// never moves it, so there is nothing for an acquire to order against, and two threads racing
/// here ask twice and store the same answer.
fn timebase() -> Ratio {
    let known = TIMEBASE.load(Ordering::Relaxed);
    if known != 0 {
        return Ratio::unpack(known);
    }
    let asked = asked_timebase();
    TIMEBASE.store(asked.packed(), Ordering::Relaxed);
    asked
}

/// The ratio the kernel answers, or one nanosecond a tick where it answers nothing.
fn asked_timebase() -> Ratio {
    let mut info = TimebaseInfo { numer: 0, denom: 0 };
    // Safety: `info` is a live, correctly typed buffer, which is all the call touches.
    if unsafe { mach_timebase_info(&raw mut info) } != KERN_SUCCESS {
        return Ratio::IDENTITY;
    }
    Ratio::new(info.numer, info.denom)
}

/// Asks the kernel for the ratio now, from the constructor, so no interposed call has to.
pub(crate) fn prime() {
    let _ratio = timebase();
}

/// The clock id a Mach clock id names, or `None` for one this platform hands out no port for.
const fn clock_of(id: libc::c_int) -> Option<libc::clockid_t> {
    match id {
        SYSTEM_CLOCK => Some(clock::MONOTONIC),
        CALENDAR_CLOCK => Some(clock::REALTIME),
        _ => None,
    }
}

/// A port as the key it is remembered under. `MACH_PORT_NULL` is zero and names no clock, so a
/// width that could not hold a port falls back on it rather than on somebody else's key.
fn key(port: libc::mach_port_t) -> usize {
    usize::try_from(port).unwrap_or(0)
}

/// A count of ticks as the program should see it, or `None` when it reads the genuine counter:
/// this process runs on genuine time, or no hook took the family over.
#[inline]
fn faked_ticks(func: Func, clock: libc::clockid_t) -> Option<u64> {
    Some(timebase().ticks(interpose::faked(func, clock)?))
}

/// Ticks of the counter that stops while the machine sleeps, which the Monotonic family holds.
#[inline]
pub(crate) fn mach_absolute_time() -> u64 {
    if let Some(ticks) = faked_ticks(Func::MachAbsoluteTime, clock::MONOTONIC) {
        return ticks;
    }
    // Safety: the genuine call takes no argument.
    unsafe { genuine::mach_absolute_time() }.unwrap_or(0)
}

/// The same counter read from the last tick rather than from the hardware.
#[cfg(darwin_mach_approximate_time)]
#[inline]
pub(crate) fn mach_approximate_time() -> u64 {
    if let Some(ticks) = faked_ticks(Func::MachApproximateTime, clock::MONOTONIC_APPROX) {
        return ticks;
    }
    // Safety: the genuine call takes no argument.
    unsafe { genuine::mach_approximate_time() }.unwrap_or(0)
}

/// Ticks of the counter that keeps counting while the machine sleeps, which Boottime holds.
#[cfg(darwin_mach_continuous_time)]
#[inline]
pub(crate) fn mach_continuous_time() -> u64 {
    if let Some(ticks) = faked_ticks(Func::MachContinuousTime, clock::BOOTTIME_RAW) {
        return ticks;
    }
    // Safety: the genuine call takes no argument.
    unsafe { genuine::mach_continuous_time() }.unwrap_or(0)
}

/// The same counter read from the last tick.
#[cfg(darwin_mach_continuous_approximate_time)]
#[inline]
pub(crate) fn mach_continuous_approximate_time() -> u64 {
    let faked = faked_ticks(
        Func::MachContinuousApproximateTime,
        clock::BOOTTIME_RAW_APPROX,
    );
    if let Some(ticks) = faked {
        return ticks;
    }
    // Safety: the genuine call takes no argument.
    unsafe { genuine::mach_continuous_approximate_time() }.unwrap_or(0)
}

/// A clock read in nanoseconds rather than in a `timespec`, under the ids `clock_gettime` takes,
/// so it follows whichever family the id it was handed belongs to.
///
/// Zero is the refusal, with the reason in `errno`, which is why a clock nothing fakes is left to
/// the genuine call rather than answered as nothing here. It is also what a faked wall clock set
/// before 1970 comes back as, since a count of nanoseconds since the epoch cannot name one; that
/// is the call's own wall and not the session's.
#[inline]
pub(crate) fn clock_gettime_nsec_np(clock: libc::clockid_t) -> u64 {
    if let Some(now) = interpose::faked(Func::ClockGettimeNsecNp, clock) {
        return u64::try_from(now.0).unwrap_or(0);
    }
    // Safety: the clock id is the caller's, passed on unchanged.
    unsafe { genuine::clock_gettime_nsec_np(clock) }.unwrap_or(0)
}

/// A sleep until the counter reads `deadline`, which is the one wait this platform measures in
/// ticks.
///
/// It converts like every other absolute deadline, with the timebase on each end: out of ticks
/// into the fake instant they come due at, through the keeper as the duration still remaining, and
/// back into the tick the genuine counter will read then.
#[inline]
pub(crate) fn mach_wait_until(deadline: u64) -> libc::kern_return_t {
    let ratio = timebase();
    let until = wait::deadline(
        Func::MachWaitUntil,
        clock::MONOTONIC,
        ratio.nanos_at(deadline),
    )
    .map_or(deadline, |genuine_at| ratio.ticks_at(genuine_at));
    // Safety: the deadline is a count of ticks, ours or the caller's.
    unsafe { genuine::mach_wait_until(until) }.unwrap_or(KERN_FAILURE)
}

/// A clock port for the Mach clock `id` names, remembered so a read on it answers from the family
/// that clock belongs to.
///
/// Reads no time itself and so reaches no hook: what it hands out is a name, and the read on that
/// name is the call that converts.
///
/// # Safety
/// `port` must be null or valid for writes of a `mach_port_t`.
#[inline]
pub(crate) unsafe fn host_get_clock_service(
    host: libc::host_t,
    id: libc::c_int,
    port: *mut libc::mach_port_t,
) -> libc::kern_return_t {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::host_get_clock_service(host, id, port) }.unwrap_or(KERN_FAILURE);
    if result != KERN_SUCCESS || port.is_null() {
        return result;
    }
    if let Some(clock) = clock_of(id) {
        // Safety: `port` is non-null, the caller guarantees it, and the call above has filled it.
        let named = unsafe { *port };
        PORTS.keep(key(named), clock);
    }
    result
}

/// A read on a clock port, which answers whichever clock the port names.
///
/// A port this process never saw handed out passes through: `clock_get_time` on a port that names
/// no clock is a refusal, and one we never named is not ours to answer for.
///
/// # Safety
/// `at` must be null or valid for writes of a `mach_timespec_t`.
#[inline]
pub(crate) unsafe fn clock_get_time(
    port: libc::mach_port_t,
    at: *mut MachTimespec,
) -> libc::kern_return_t {
    if let Some(clock) = PORTS.kept(key(port))
        && !at.is_null()
        && let Some(now) = interpose::faked(Func::ClockGetTime, clock)
    {
        // Safety: `at` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe { *at = MachTimespec::of(now) };
        return KERN_SUCCESS;
    }
    // Safety: both arguments are the caller's, passed on unchanged.
    unsafe { genuine::clock_get_time(port, at) }.unwrap_or(KERN_FAILURE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timebase_is_the_one_the_kernel_answers() {
        let ratio = timebase();
        assert_eq!(
            ratio,
            asked_timebase(),
            "the remembered pair is the asked one"
        );
        // A second of the counter is a second of the clock it stands for, whichever machine this
        // is: 1:1 on Intel and 125:3 on Apple silicon.
        assert_eq!(
            ratio.nanos(ratio.ticks(Nanos(60 * Nanos::SECOND))).0 / 1_000_000,
            60_000
        );
    }

    #[test]
    fn the_two_mach_clock_ids_are_the_families_of_the_clocks_they_read() {
        assert_eq!(clock_of(SYSTEM_CLOCK), Some(clock::MONOTONIC));
        assert_eq!(clock_of(CALENDAR_CLOCK), Some(clock::REALTIME));
        // `REALTIME_CLOCK` is `SYSTEM_CLOCK`, whatever the name suggests.
        assert_eq!(clock_of(0), Some(clock::MONOTONIC));
        // And an id naming neither is a port we answer for nobody.
        assert_eq!(clock_of(2), None);
        assert_eq!(clock_of(-1), None);
    }

    #[test]
    fn a_port_reads_the_clock_it_was_handed_out_for_and_an_unknown_one_reads_nothing() {
        let ports = clocks::Table::new();
        ports.keep(key(0x707), clock::REALTIME);
        ports.keep(key(0x903), clock::MONOTONIC);
        assert_eq!(ports.kept(key(0x707)), Some(clock::REALTIME));
        assert_eq!(ports.kept(key(0x903)), Some(clock::MONOTONIC));
        assert_eq!(ports.kept(key(0x111)), None);
    }

    #[test]
    fn an_instant_outside_what_machs_own_pair_holds_is_clamped_into_it() {
        let stamped = MachTimespec::of(Nanos(1_577_836_800 * Nanos::SECOND + 250));
        assert_eq!(stamped.tv_sec, 1_577_836_800);
        assert_eq!(stamped.tv_nsec, 250);
        // Before the epoch and past 2106, the two ends the narrower pair has.
        assert_eq!(MachTimespec::of(Nanos(-Nanos::SECOND)).tv_sec, 0);
        assert_eq!(MachTimespec::of(Nanos(i64::MAX)).tv_sec, u32::MAX);
        // The fraction stays inside its second either way, as it does in a `timespec`.
        assert!(MachTimespec::of(Nanos(-1)).tv_nsec >= 0);
    }
}
