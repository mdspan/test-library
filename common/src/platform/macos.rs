//! macOS.

use std::ffi::c_void;
use std::io;
use std::ptr;

use crate::time::ticks::{self, Times};
use crate::time::{Family, Nanos};

/// What the header's platform field carries for macOS.
pub const OS_TAG: u32 = 3;

/// The clock ids of the families, by the names the family table gives them.
///
/// The two elapsed names are the reverse of Linux's: `CLOCK_MONOTONIC` keeps counting while the
/// machine sleeps and belongs to Boottime, and `CLOCK_UPTIME_RAW` stops and belongs to Monotonic.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_UPTIME_RAW;
    /// The same, read from the last tick rather than from the counter.
    pub const MONOTONIC_APPROX: clockid_t = libc::CLOCK_UPTIME_RAW_APPROX;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_MONOTONIC;
    /// The same, without the NTP slew.
    pub const BOOTTIME_RAW: clockid_t = libc::CLOCK_MONOTONIC_RAW;
    /// The same again, read from the last tick.
    pub const BOOTTIME_RAW_APPROX: clockid_t = libc::CLOCK_MONOTONIC_RAW_APPROX;
    /// The wall clock without leap seconds, which macOS does not keep separately.
    pub const TAI: clockid_t = libc::CLOCK_REALTIME;
    /// CPU time consumed by the whole process.
    pub const PROCESS_CPUTIME: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    /// CPU time consumed by the calling thread.
    pub const THREAD_CPUTIME: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    /// Every clock a family holds, by the name a user writes for a timebase.
    ///
    /// The `mach_*_time` functions belong to these families too, and are not here: they are clocks
    /// without a clock id, so there is no id for a user to name one with.
    const NAMED: &[(&str, clockid_t)] = &[
        ("CLOCK_REALTIME", REALTIME),
        ("CLOCK_UPTIME_RAW", MONOTONIC),
        ("CLOCK_UPTIME_RAW_APPROX", MONOTONIC_APPROX),
        ("CLOCK_MONOTONIC", BOOTTIME),
        ("CLOCK_MONOTONIC_RAW", BOOTTIME_RAW),
        ("CLOCK_MONOTONIC_RAW_APPROX", BOOTTIME_RAW_APPROX),
        ("CLOCK_PROCESS_CPUTIME_ID", PROCESS_CPUTIME),
        ("CLOCK_THREAD_CPUTIME_ID", THREAD_CPUTIME),
    ];

    /// The clocks a user may name as a timebase. One list, because no clock here moves between the
    /// families with the kernel's version.
    #[must_use]
    pub const fn named() -> &'static [(&'static str, clockid_t)] {
        NAMED
    }

    /// The family a clock id belongs to, or `None` when the id is left on genuine time.
    #[must_use]
    pub const fn family_of(clock: clockid_t) -> Option<Family> {
        match clock {
            REALTIME => Some(Family::Realtime),
            MONOTONIC | MONOTONIC_APPROX => Some(Family::Monotonic),
            BOOTTIME | BOOTTIME_RAW | BOOTTIME_RAW_APPROX => Some(Family::Boottime),
            PROCESS_CPUTIME | THREAD_CPUTIME => Some(Family::Cpu),
            _ => None,
        }
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    ///
    /// Never here: macOS has no clock id of its own for TAI, so there is none to carry it.
    #[must_use]
    pub const fn is_tai(_clock: clockid_t) -> bool {
        false
    }
}

/// The first clock in the family's column of the family table.
///
/// A family's zero always comes from it, and with no override it is the timebase too. Cpu has
/// none, because each of its clocks tracks passage through its own reading.
#[must_use]
pub const fn default_timebase(family: Family) -> Option<libc::clockid_t> {
    match family {
        Family::Realtime => Some(clock::REALTIME),
        Family::Monotonic => Some(clock::MONOTONIC),
        Family::Boottime => Some(clock::BOOTTIME),
        Family::Cpu => None,
    }
}

/// The id the kernel gives this thread.
///
/// What names a thread where thread-local storage cannot: a runtime that makes its own threads
/// without libc leaves the thread pointer holding something of its own.
#[must_use]
pub fn thread_id() -> u64 {
    let mut id: u64 = 0;
    // Safety: a null thread names the calling one, and the id lands in a live word.
    if unsafe { libc::pthread_threadid_np(0, &raw mut id) } != 0 {
        return 0;
    }
    id
}

/// Sets this thread's `errno`, which is how a refused call reports itself in libc's convention.
pub fn set_errno(value: libc::c_int) {
    // Safety: `__error` answers this thread's own `errno`, taking nothing.
    let location = unsafe { libc::__error() };
    // Safety: the location libc just named is live for as long as this thread is.
    unsafe { *location = value };
}

// Every genuine function here is a call through this library's own import of the name: dyld binds
// the imports of the image that carries the interpose table to the functions the table replaces,
// and every other image's to the replacements.
//
// A call one library of the shared cache makes into another goes through a pointer on `x86_64`,
// and dyld points those at the replacements as it does the program's. So where libSystem builds a
// genuine function out of another name the table carries, it is built here out of the same calls.
unsafe extern "C" {
    /// Ticks of the counter that stops while the machine sleeps.
    fn mach_absolute_time() -> u64;
    /// The same counter read from the last tick.
    fn mach_approximate_time() -> u64;
    /// Ticks of the counter that keeps counting while the machine sleeps.
    fn mach_continuous_time() -> u64;
    /// The same counter read from the last tick.
    fn mach_continuous_approximate_time() -> u64;
    /// The ratio the counters convert to nanoseconds with.
    fn mach_timebase_info(info: *mut TimebaseInfo) -> libc::kern_return_t;
}

/// What `mach_timebase_info` fills. Spelled here rather than taken from libc's bindings, which
/// mark every Mach name deprecated in favour of a crate this workspace does not depend on.
#[derive(Debug)]
#[repr(C)]
struct TimebaseInfo {
    /// Nanoseconds per `denom` ticks.
    numer: u32,
    /// Ticks per `numer` nanoseconds.
    denom: u32,
}

/// The ticks a second holds in what `times` answers: `CLK_TCK` in libSystem's headers.
const CLK_TCK: i64 = 100;

/// Nothing to settle: every genuine function here is an import of this library's own, which dyld
/// bound before any code of the process ran.
pub const fn prime_genuine() {}

/// Reads a genuine clock. libc's convention.
///
/// libSystem's `clock_gettime` reads the four counter clocks through `mach_absolute_time` and its
/// three neighbours, and the process's CPU time through `getrusage`, each in another library of the
/// cache. Those five are read here from the same calls, converted the way libSystem converts them,
/// and the rest are left to libSystem, which builds them from nothing the table carries.
///
/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[must_use]
pub unsafe fn genuine_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    let counter: unsafe extern "C" fn() -> u64 = match clock {
        clock::MONOTONIC => mach_absolute_time,
        clock::MONOTONIC_APPROX => mach_approximate_time,
        clock::BOOTTIME_RAW => mach_continuous_time,
        clock::BOOTTIME_RAW_APPROX => mach_continuous_approximate_time,
        clock::PROCESS_CPUTIME => {
            let Some(usage) = rusage(libc::RUSAGE_SELF) else {
                return -1;
            };
            let used = Nanos::from_timeval(&usage.ru_utime)
                .saturating_add(Nanos::from_timeval(&usage.ru_stime));
            // Safety: the caller guarantees `ts`.
            return unsafe { answer(used, ts) };
        }
        // Safety: the caller guarantees `ts`, which passes on unchanged.
        _ => return unsafe { libc::clock_gettime(clock, ts) },
    };
    let mut timebase = TimebaseInfo { numer: 0, denom: 0 };
    // Safety: `timebase` is a live, correctly typed buffer, which is all the call touches.
    if unsafe { mach_timebase_info(&raw mut timebase) } != 0 || timebase.denom == 0 {
        set_errno(libc::EINVAL);
        return -1;
    }
    // Safety: the counter takes no argument.
    let ticks = unsafe { counter() };
    let nanos = u128::from(ticks) * u128::from(timebase.numer) / u128::from(timebase.denom);
    // Safety: the caller guarantees `ts`.
    unsafe { answer(Nanos(i64::try_from(nanos).unwrap_or(i64::MAX)), ts) }
}

/// Writes a reading into the caller's `timespec`. A null one is refused with the `EFAULT` the
/// kernel gives a buffer it cannot write.
///
/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
unsafe fn answer(now: Nanos, ts: *mut libc::timespec) -> libc::c_int {
    if ts.is_null() {
        set_errno(libc::EFAULT);
        return -1;
    }
    // Safety: `ts` is non-null, so the caller's guarantee makes it valid for this write.
    unsafe { *ts = now.to_timespec() };
    0
}

/// The genuine resource usage of `who`, or `None` when the kernel refuses it.
fn rusage(who: libc::c_int) -> Option<libc::rusage> {
    // Safety: a `rusage` is integers and `timeval`s, for which all zeroes is a value.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // Safety: `usage` is a live, correctly typed buffer.
    (unsafe { libc::getrusage(who, &raw mut usage) } == 0).then_some(usage)
}

/// Reads the genuine process times `times(3)` reports, or `None` with `errno` set when a call it is
/// built from is refused.
///
/// Built here the way libSystem builds it, out of two `getrusage` calls and one `gettimeofday`,
/// since libSystem's own `getrusage` call crosses into another library of the cache. The elapsed
/// ticks count from the epoch rather than from boot, as libSystem's do.
#[must_use]
pub fn genuine_times() -> Option<Times> {
    let own = rusage(libc::RUSAGE_SELF)?;
    let children = rusage(libc::RUSAGE_CHILDREN)?;
    let mut now = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    // Safety: `now` is a live buffer, and the timezone may be null.
    if unsafe { libc::gettimeofday(&raw mut now, ptr::null_mut()) } != 0 {
        return None;
    }
    Some(Times {
        user: ticks::of_timeval(&own.ru_utime, CLK_TCK),
        system: ticks::of_timeval(&own.ru_stime, CLK_TCK),
        children_user: ticks::of_timeval(&children.ru_utime, CLK_TCK),
        children_system: ticks::of_timeval(&children.ru_stime, CLK_TCK),
        elapsed: ticks::of_timeval(&now, CLK_TCK),
    })
}

/// Reads the genuine resource usage of `who`. libc's convention.
///
/// # Safety
/// `usage` must be valid for writes of a `rusage`.
#[must_use]
pub unsafe fn genuine_getrusage(who: libc::c_int, usage: *mut libc::rusage) -> libc::c_int {
    // Safety: the caller guarantees `usage`, which passes on unchanged.
    unsafe { libc::getrusage(who, usage) }
}

/// Sets the genuine wall clock, libc's convention, for a process this library is not faking.
///
/// A null `tv` leaves the clock where it is and a null `tz` leaves the obsolete timezone alone,
/// which is what the kernel makes of each, so both pass through as the program wrote them.
///
/// # Safety
/// `tv` must be null or valid for reads of a `timeval`, and `tz` null or valid for reads of a
/// `timezone`.
#[must_use]
pub unsafe fn genuine_settimeofday(tv: *const libc::timeval, tz: *const c_void) -> libc::c_int {
    // Safety: the caller guarantees both, which pass on unchanged.
    unsafe { libc::settimeofday(tv, tz.cast()) }
}

/// Sets a genuine clock, libc's convention, for a process this library is not faking.
///
/// # Safety
/// `tp` must be valid for reads of a `timespec`.
#[must_use]
pub unsafe fn genuine_clock_settime(
    clock: libc::clockid_t,
    tp: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller guarantees `tp`, which passes on unchanged.
    unsafe { libc::clock_settime(clock, tp) }
}

/// Makes the genuine one-shot offset adjustment, libc's convention, for a process this library is
/// not faking.
///
/// # Safety
/// `delta` must be valid for reads of a `timeval`, and `olddelta` null or valid for writes of one.
#[must_use]
pub unsafe fn genuine_adjtime(
    delta: *const libc::timeval,
    olddelta: *mut libc::timeval,
) -> libc::c_int {
    // Safety: the caller guarantees both, which pass on unchanged.
    unsafe { libc::adjtime(delta, olddelta) }
}

/// The widest delta the platform's own `adjtime` takes before it refuses, in seconds.
///
/// `adjtime` is a system call of its own here, taking the `timeval` as it comes, so the bound is
/// the range an instant holds rather than anything libc imposes.
pub const ADJTIME_SECONDS: i64 = i64::MAX / 1_000_000_000;

/// The order a set of the clock is checked in here.
///
/// libSystem's `clock_settime` refuses any clock but the wall clock before it makes a call, and the
/// kernel then asks for privilege before it copies the value in or looks at it. A process without
/// privilege hears `EPERM` for any set of the wall clock, malformed or not.
pub const SET_ORDER: [super::SetCheck; 4] = [
    super::SetCheck::Clock,
    super::SetCheck::Privilege,
    super::SetCheck::Pointer,
    super::SetCheck::Value,
];

/// Whether `adjtime` with no delta is a question that needs no privilege: it is not, since the
/// kernel asks for privilege first and then reads the delta, which a null pointer makes `EFAULT`.
pub const ADJTIME_NULL_ASKS: bool = false;

/// The units `clock` answers in: the million `CLOCKS_PER_SEC` names on an XSI system.
pub const CLOCKS_PER_SEC: i64 = 1_000_000;

/// Whether the kernel would let this process set the clock: an effective uid of 0 here, since
/// macOS gates the call on the superuser rather than on a capability.
#[must_use]
pub fn may_set_the_clock() -> bool {
    // Safety: `geteuid` reads process state and cannot fail.
    let euid = unsafe { libc::geteuid() };
    euid == 0
}

/// `fcntl`'s open-file-description lock, waiting for the current holder. The kernel has had these
/// since 10.11, but an SDK older than macOS 14 hides the definitions, so the numbers are spelled
/// here rather than taken from bindings that follow a header which may not have them.
const F_OFD_SETLKW: libc::c_int = 91;
/// The question of whether such a lock is held.
const F_OFD_GETLK: libc::c_int = 92;

/// Takes the write lock on `fd`: an exclusive open-file-description lock on byte 0, blocking until
/// the current holder releases it. libc's convention, so a caller retries `EINTR` itself.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file, opened for writing.
#[must_use]
pub unsafe fn take_write_lock(fd: libc::c_int) -> libc::c_int {
    let request = crate::sync::lock_request();
    // Safety: the caller guarantees `fd`, and `request` is a live `flock`.
    unsafe { libc::fcntl(fd, F_OFD_SETLKW, &raw const request) }
}

/// Asks the kernel whether the write lock on `fd` is held, taking nothing. Fills `request`, which
/// the caller has zeroed and filled with the question.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file and `request` a live `flock`.
#[must_use]
pub unsafe fn probe_write_lock(fd: libc::c_int, request: *mut libc::flock) -> libc::c_int {
    // Safety: the caller guarantees both arguments.
    unsafe { libc::fcntl(fd, F_OFD_GETLK, request) }
}

/// How much randomness one `getentropy(2)` answers at most.
const ENTROPY_AT_ONCE: usize = 256;

/// Fills `buf` with randomness from the kernel.
///
/// # Errors
/// Whatever `getentropy(2)` refused with.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    for chunk in buf.chunks_mut(ENTROPY_AT_ONCE) {
        // Safety: the pointer and length name a live buffer of no more than one call's worth.
        let got = unsafe { libc::getentropy(chunk.as_mut_ptr().cast::<c_void>(), chunk.len()) };
        if got != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_table_places_every_clock_of_the_darwin_column() {
        for (id, family) in [
            (clock::REALTIME, Family::Realtime),
            (clock::MONOTONIC, Family::Monotonic),
            (clock::MONOTONIC_APPROX, Family::Monotonic),
            (clock::BOOTTIME, Family::Boottime),
            (clock::BOOTTIME_RAW, Family::Boottime),
            (clock::BOOTTIME_RAW_APPROX, Family::Boottime),
            (clock::PROCESS_CPUTIME, Family::Cpu),
            (clock::THREAD_CPUTIME, Family::Cpu),
        ] {
            assert_eq!(clock::family_of(id), Some(family), "clock {id}");
        }
    }

    #[test]
    fn the_two_elapsed_names_are_the_reverse_of_the_linux_ones() {
        // `CLOCK_MONOTONIC` keeps counting while the machine sleeps here, so it is the Boottime
        // family's, and `CLOCK_UPTIME_RAW` stops, so it is Monotonic's.
        assert_eq!(
            clock::family_of(libc::CLOCK_MONOTONIC),
            Some(Family::Boottime)
        );
        assert_eq!(
            clock::family_of(libc::CLOCK_UPTIME_RAW),
            Some(Family::Monotonic)
        );
        // And the timebases follow: Monotonic tracks `CLOCK_UPTIME_RAW`, Boottime tracks
        // `CLOCK_MONOTONIC`, and Realtime tracks the wall clock.
        assert_eq!(default_timebase(Family::Realtime), Some(clock::REALTIME));
        assert_eq!(
            default_timebase(Family::Monotonic),
            Some(libc::CLOCK_UPTIME_RAW)
        );
        assert_eq!(
            default_timebase(Family::Boottime),
            Some(libc::CLOCK_MONOTONIC)
        );
        assert_eq!(default_timebase(Family::Cpu), None);
    }

    #[test]
    fn there_is_no_clock_carrying_the_tai_offset() {
        for &(_, id) in clock::named() {
            assert!(!clock::is_tai(id), "clock {id}");
        }
    }

    #[test]
    fn only_the_superuser_may_set_the_clock() {
        // Safety: `geteuid` reads process state and cannot fail.
        let euid = unsafe { libc::geteuid() };
        assert_eq!(
            may_set_the_clock(),
            euid == 0,
            "the clock is the superuser's to set here, and nobody else's"
        );
    }

    /// A genuine reading of `id` through the platform's own function.
    fn ours(id: libc::clockid_t) -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        let read = unsafe { genuine_clock_gettime(id, &raw mut ts) };
        assert_eq!(read, 0, "clock {id}");
        Nanos::from_timespec(&ts).0
    }

    /// libSystem's own reading of `id`, which is genuine in a process with no interpose table.
    fn libsystem(id: libc::clockid_t) -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        let read = unsafe { libc::clock_gettime(id, &raw mut ts) };
        assert_eq!(read, 0, "clock {id}");
        Nanos::from_timespec(&ts).0
    }

    #[test]
    fn every_clock_this_platform_names_reads() {
        prime_genuine();
        assert!(
            ours(clock::REALTIME) > 1_700_000_000 * Nanos::SECOND,
            "the wall clock read {}",
            ours(clock::REALTIME)
        );
        for &(_, id) in clock::named() {
            ours(id);
        }
    }

    #[test]
    fn the_clocks_read_from_the_calls_underneath_read_what_libsystem_reads() {
        // Every one of these only moves forward, so a reading of ours between two of libSystem's
        // is the same reading.
        for id in [
            clock::MONOTONIC,
            clock::MONOTONIC_APPROX,
            clock::BOOTTIME_RAW,
            clock::BOOTTIME_RAW_APPROX,
            clock::PROCESS_CPUTIME,
        ] {
            let before = libsystem(id);
            let read = ours(id);
            let after = libsystem(id);
            assert!(
                (before..=after).contains(&read),
                "clock {id} read {read}, outside {before}..={after}"
            );
        }
    }

    #[test]
    fn the_process_times_are_the_ones_libsystem_answers() {
        let empty = libc::tms {
            tms_utime: 0,
            tms_stime: 0,
            tms_cutime: 0,
            tms_cstime: 0,
        };
        let (mut before, mut after) = (empty, empty);
        // Safety: each buffer is a live, correctly typed `tms`.
        let elapsed_before = unsafe { libc::times(&raw mut before) };
        let genuine = genuine_times().expect("the calls times is built from answer");
        // Safety: as above.
        let elapsed_after = unsafe { libc::times(&raw mut after) };
        let elapsed = ticks::narrow::<libc::clock_t>(genuine.elapsed);
        let read = genuine.tms();
        assert!(
            (elapsed_before..=elapsed_after).contains(&elapsed),
            "elapsed {elapsed}, outside {elapsed_before}..={elapsed_after}"
        );
        for (field, low, mid, high) in [
            (
                "tms_utime",
                before.tms_utime,
                read.tms_utime,
                after.tms_utime,
            ),
            (
                "tms_stime",
                before.tms_stime,
                read.tms_stime,
                after.tms_stime,
            ),
            (
                "tms_cutime",
                before.tms_cutime,
                read.tms_cutime,
                after.tms_cutime,
            ),
            (
                "tms_cstime",
                before.tms_cstime,
                read.tms_cstime,
                after.tms_cstime,
            ),
        ] {
            assert!(
                (low..=high).contains(&mid),
                "{field} {mid}, outside {low}..={high}"
            );
        }
    }

    #[test]
    fn a_refused_call_reports_itself_the_way_libc_does() {
        // Safety: an unknown clock id is refused before the kernel reads the buffer.
        let refused = unsafe { genuine_clock_gettime(0x7fff_ffff, ptr::null_mut()) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn a_counter_read_into_no_buffer_is_refused_as_a_bad_address() {
        // Safety: a null buffer is refused before anything is written through it.
        let refused = unsafe { genuine_clock_gettime(clock::MONOTONIC, ptr::null_mut()) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EFAULT)
        );
    }
}
