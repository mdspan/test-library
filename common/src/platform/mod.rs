//! The platform seam.
//!
//! Everything above this module is portable. Each arm supplies the same items:
//!
//! | Item | Contract |
//! |---|---|
//! | `OS_TAG` | The value the session header's platform field carries for this OS. |
//! | `clock` | The clock ids of the families, the names a user writes for them, which family each id belongs to, and which id carries the TAI offset. |
//! | `default_timebase` | The first clock in a family's column of the family table. FreeBSD answers from the running kernel, because the names crossed over at 15.0. |
//! | `genuine_clock_gettime` | Reads a genuine clock into the caller's `timespec`, libc's convention. Async-signal-safe, and never routed through a symbol we interpose. |
//! | `prime_genuine` | Settles, from the constructor, how `genuine_clock_gettime` reads: the kernel's vDSO on Linux, libc on FreeBSD. Nothing on macOS, where dyld settles it at load. |
//! | `genuine_times` | Reads the genuine CPU times and elapsed ticks `times` reports, as [`Times`](crate::time::ticks::Times) counts 64 bits wide, or `None` with `errno` set. Never routed through a symbol we interpose. |
//! | `genuine_getrusage` | Reads the genuine `rusage` of a target, libc's convention. Never routed through a symbol we interpose. |
//! | `genuine_settimeofday`, `genuine_clock_settime`, `genuine_adjtime` | Set the genuine clock, libc's convention, for a process this library is not faking. |
//! | `may_set_the_clock` | Whether the kernel would let this process set the clock genuinely: asked of the kernel with a call it refuses for privilege first and sets nothing with on Linux and FreeBSD, effective uid 0 on macOS. Leaves `errno` alone. |
//! | `SET_ORDER` | The order the kernel makes the four checks of a set of the clock in. |
//! | `ADJTIME_NULL_ASKS` | Whether `adjtime` with no delta is a question that needs no privilege. |
//! | `ADJTIME_SECONDS` | The widest delta the platform's own `adjtime` takes before it refuses. |
//! | `CLOCKS_PER_SEC` | The units `clock` answers in, which the platform's headers fix. |
//! | `set_errno` | Sets this thread's `errno`, which is how a refused call reports itself. |
//! | `thread_id` | The id the kernel gives the calling thread, taking no thread-local storage to answer. |
//! | `hold_signals` | Runs a closure that may overwrite the register a Go runtime's signal handler takes its goroutine pointer from, with every signal held back where that handler can be reached through a synthetic vDSO entry: Linux on aarch64. Elsewhere it runs the closure as it stands. |
//! | `take_write_lock` | Takes the write lock on a descriptor, blocking. libc's convention. |
//! | `probe_write_lock` | Asks the kernel whether the write lock is held, taking nothing. |
//! | `random_bytes` | Fills a buffer with randomness, for the session name. |
//!
//! [`genuine_symbol`] is the way back to a genuine function on Linux and FreeBSD, where the
//! loader answers it the same way. macOS reaches the functions in the table above through this
//! library's own imports of their names instead, which dyld binds past the interpose table.
//!
//! An arm may hold more than the table: Linux also supplies `genuine_syscall`, because `syscall()`
//! is one of the calls we take over there, and only that arm's interposed functions name it,
//! `kernel_clock_gettime` with `kernel_gettimeofday`, which answer in the convention a vDSO entry
//! owes its caller in place of libc's, and `AT_SYSINFO_EHDR`, the entry naming the vDSO. FreeBSD
//! supplies `AT_TIMEKEEP`, the entry naming the timekeep page, which the Go fallback interposition
//! withholds and no other platform has.

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

use crate::time::{Family, Nanos};

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
        pub use linux::*;
    } else if #[cfg(target_os = "freebsd")] {
        mod freebsd;
        pub use freebsd::*;
    } else if #[cfg(target_os = "macos")] {
        mod macos;
        pub use macos::*;
    } else {
        compile_error!(
            "libtimewarp supports Linux, macOS, and FreeBSD. Add an arm under common/src/platform \
             supplying the items in the table in platform/mod.rs."
        );
    }
}

/// The architecture half of the header's platform tag.
pub const ARCH_TAG: u32 = if cfg!(target_arch = "x86_64") {
    1
} else if cfg!(target_arch = "aarch64") {
    2
} else {
    0
};

/// What the header's platform field carries: which OS's clock ids the parameters speak, and which
/// architecture wrote them.
pub const PLATFORM_TAG: u32 = (OS_TAG << 16) | ARCH_TAG;

/// Converts a clock id between the platform's own `clockid_t` and the signed 32-bit word the
/// session header and the hook ABI spell one with.
///
/// `clockid_t` is signed on Linux and FreeBSD and unsigned on macOS, so both directions go through
/// `TryFrom`. Every id converted comes from a platform table or from the program's own call, which
/// makes the fallback unreachable.
pub fn clock_id<U, T: TryFrom<U> + Default>(value: U) -> T {
    T::try_from(value).unwrap_or_default()
}

/// The clock id a user's timebase name refers to.
#[must_use]
pub fn clock_named(name: &str) -> Option<libc::clockid_t> {
    clock::named()
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, id)| id)
}

/// The clock the Monotonic family tracks here.
///
/// Asked rather than spelled, because FreeBSD picks between two columns of the family table by the
/// running kernel's version, and the name `CLOCK_MONOTONIC` crossed from one to the other at 15.0.
/// Cpu is the one family with no timebase, and this is not it.
#[must_use]
pub fn monotonic_timebase() -> libc::clockid_t {
    default_timebase(Family::Monotonic).unwrap_or(clock::REALTIME)
}

/// Whether `family` may track passage against `clock`.
///
/// Any clock of the Monotonic or Boottime family serves every family. `CLOCK_REALTIME` serves the
/// Realtime family alone: a step in it reaching a monotonic family would break its monotonicity.
#[must_use]
pub fn timebase_allowed(family: Family, clock: libc::clockid_t) -> bool {
    match clock::family_of(clock) {
        Some(Family::Monotonic | Family::Boottime) => true,
        Some(Family::Realtime) => family == Family::Realtime && clock == clock::REALTIME,
        _ => false,
    }
}

/// Whether the kernel would let `clock_settime` name this clock of the family table at all.
///
/// The wall clock alone, on all three platforms: every other clock a family holds is one the
/// kernel derives rather than keeps, so naming it is `EINVAL` however privileged the caller is. A
/// clock no family holds is never asked about, because a set of it goes to the genuine function.
#[must_use]
pub const fn settable(clock: libc::clockid_t) -> bool {
    clock == clock::REALTIME
}

/// One of the things a kernel checks a set of the clock for.
///
/// All three check the same four, each in an order of its own, [`SET_ORDER`], and a call that
/// fails more than one is refused for whichever comes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetCheck {
    /// The clock is one that can be set at all, `EINVAL` when it is not: [`settable`].
    Clock,
    /// The new value is somewhere the kernel can read it, `EFAULT` when it is not.
    Pointer,
    /// This process may set the clock, `EPERM` when it may not: [`may_set_the_clock`].
    Privilege,
    /// The new value is one the call may carry, `EINVAL` when it is not.
    Value,
}

/// The loader's name for "the definition after the one asking": all ones on Linux, macOS, and
/// FreeBSD alike. The libc crate spells it for the BSDs and not for Linux, so it is spelled here
/// once for all three.
pub(crate) const RTLD_NEXT: *mut c_void = std::ptr::without_provenance_mut(usize::MAX);

/// The definition of `name` past ours: the one the loader would have bound the program to had we
/// never been loaded.
///
/// The waits need it: most of them are libc's own constructions over futexes, which leave no
/// system call to make in their place, so the genuine function comes from the loader. The answer
/// never moves, so a caller looks it up once and remembers it.
///
/// # Safety
/// `name` must be NUL-terminated.
#[must_use]
pub unsafe fn genuine_symbol(name: &[u8]) -> Option<NonNull<c_void>> {
    // Safety: the caller guarantees the name, and `RTLD_NEXT` is a sentinel the loader compares
    // rather than a pointer it reads.
    let found = unsafe { libc::dlsym(RTLD_NEXT, name.as_ptr().cast::<c_char>()) };
    if found.is_null() {
        // `dlerror` reports and clears, so leaving ours set would hand the program our message the
        // next time it asked about a failure of its own.
        // Safety: `dlerror` takes nothing and reports on the calling thread alone.
        let _reported = unsafe { libc::dlerror() };
        return None;
    }
    NonNull::new(found)
}

/// Reads a genuine clock. `None` only when the kernel refuses the id.
#[must_use]
pub fn genuine_nanos(clock: libc::clockid_t) -> Option<Nanos> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a live, correctly typed buffer.
    if unsafe { genuine_clock_gettime(clock, &raw mut ts) } != 0 {
        return None;
    }
    Some(Nanos::from_timespec(&ts))
}

// Every arm answers these. Each also carries the cases for its own family table.
#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    /// The three clocks a family's line starts on, which is what a portable case can name. Only
    /// Cpu has none, and the case below is what holds that to be so.
    fn family_clocks() -> [libc::clockid_t; 3] {
        [Family::Realtime, Family::Monotonic, Family::Boottime]
            .map(|family| default_timebase(family).unwrap_or(clock::REALTIME))
    }

    /// The Boottime clock that reads the Monotonic timebase's counter plus the time suspended.
    ///
    /// The Boottime timebase itself, except on macOS: libSystem computes `CLOCK_MONOTONIC` there as
    /// the wall clock less `kern.boottime`, which the kernel records as the wall-clock time it first
    /// set the calendar at, after the counter started. That clock has a later zero than the counter
    /// and follows every slew of the wall clock, so `CLOCK_MONOTONIC_RAW` is the one that counts on.
    #[cfg(target_os = "macos")]
    const fn counting_on(_boottime: libc::clockid_t) -> libc::clockid_t {
        clock::BOOTTIME_RAW
    }

    /// The same, where that clock is the Boottime timebase.
    #[cfg(not(target_os = "macos"))]
    const fn counting_on(boottime: libc::clockid_t) -> libc::clockid_t {
        boottime
    }

    #[test]
    fn the_genuine_clocks_read() {
        let [realtime, monotonic, boottime] = family_clocks();
        assert!(genuine_nanos(realtime).unwrap() > Nanos(1_700_000_000 * Nanos::SECOND));
        let elapsed = genuine_nanos(monotonic).unwrap();
        assert!(elapsed >= Nanos::ZERO);
        assert!(genuine_nanos(boottime).unwrap() >= Nanos::ZERO);
        // Read second, from the same counter plus the time suspended, so it can only be at or above
        // the first.
        assert!(genuine_nanos(counting_on(boottime)).unwrap() >= elapsed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_call_by_number_answers_the_way_libc_would() {
        // Safety: `getpid` takes no argument and touches nothing of ours.
        let pid = unsafe { genuine_syscall(libc::SYS_getpid, [0; SYSCALL_ARGS]) };
        assert_eq!(pid, i64::from(std::process::id()));

        // A number the kernel has never heard of reports itself the way libc reports one.
        // Safety: an unknown number is refused before the kernel reads an argument.
        assert_eq!(unsafe { genuine_syscall(-1, [0; SYSCALL_ARGS]) }, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOSYS)
        );
    }

    #[test]
    fn the_genuine_process_times_and_usage_read() {
        let first = genuine_times().expect("times reads");
        assert!(first.elapsed > 0, "times answered {first:?}");
        let second = genuine_times().expect("times reads again");
        assert!(
            second.elapsed >= first.elapsed,
            "{second:?} before {first:?}"
        );
        assert!(second.user >= first.user && second.system >= first.system);

        // Safety: `rusage` is plain data.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // Safety: `usage` is a live, correctly typed buffer.
        let read = unsafe { genuine_getrusage(libc::RUSAGE_SELF, &raw mut usage) };
        assert_eq!(read, 0);
        // The times of a process this short may still round to zero, so the field this reads to
        // prove the buffer was filled is the one that is never zero.
        assert!(usage.ru_maxrss > 0, "getrusage filled nothing");
        assert!(usage.ru_utime.tv_sec >= 0 && usage.ru_utime.tv_usec >= 0);
        // A target the kernel does not know is refused, libc's way.
        // Safety: as above, with a target the kernel refuses.
        let refused = unsafe { genuine_getrusage(4_242, &raw mut usage) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn the_loader_finds_the_definition_past_ours() {
        // Safety: both names are NUL-terminated.
        let found = unsafe { genuine_symbol(b"nanosleep\0") };
        assert!(found.is_some(), "libc defines nanosleep");
        // Safety: as above.
        let missing = unsafe { genuine_symbol(b"libtimewarp_no_such_symbol\0") };
        assert_eq!(missing, None);
        // The message the failure left is cleared, so the program's next `dlerror` is its own.
        // Safety: `dlerror` takes nothing and reports on this thread alone.
        assert!(unsafe { libc::dlerror() }.is_null());
    }

    #[test]
    fn an_unknown_clock_id_fails_rather_than_lying() {
        assert_eq!(genuine_nanos(0x7fff_ffff), None);
        assert_eq!(clock::family_of(0x7fff_ffff), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_family_table_places_every_clock_of_the_linux_column() {
        for (id, family) in [
            (clock::REALTIME, Family::Realtime),
            (clock::REALTIME_COARSE, Family::Realtime),
            (clock::REALTIME_ALARM, Family::Realtime),
            (clock::TAI, Family::Realtime),
            (clock::MONOTONIC, Family::Monotonic),
            (clock::MONOTONIC_COARSE, Family::Monotonic),
            (clock::MONOTONIC_RAW, Family::Monotonic),
            (clock::BOOTTIME, Family::Boottime),
            (clock::BOOTTIME_ALARM, Family::Boottime),
            (clock::PROCESS_CPUTIME, Family::Cpu),
            (clock::THREAD_CPUTIME, Family::Cpu),
        ] {
            assert_eq!(clock::family_of(id), Some(family), "clock {id}");
        }
    }

    #[test]
    fn every_named_clock_belongs_to_a_family_and_reads() {
        // Linux's two alarm clocks need CAP_WAKE_ALARM; without it the kernel refuses the id
        // outright. FreeBSD refuses CLOCK_TAI until a time daemon has told the kernel the TAI
        // offset. So a reading is only demanded of the clocks the kernel always answers.
        #[cfg(target_os = "linux")]
        let may_refuse = [clock::REALTIME_ALARM, clock::BOOTTIME_ALARM];
        #[cfg(target_os = "freebsd")]
        let may_refuse = [clock::TAI];
        #[cfg(target_os = "macos")]
        let may_refuse: [libc::clockid_t; 0] = [];
        for &(name, id) in clock::named() {
            assert_eq!(clock_named(name), Some(id));
            assert!(clock::family_of(id).is_some(), "{name}");
            if !may_refuse.contains(&id) {
                assert!(genuine_nanos(id).is_some(), "{name}");
            }
        }
        assert_eq!(clock_named("CLOCK_YESTERDAY"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_clocks_that_measure_the_outside_world_pass_through() {
        // Dynamic clocks and the ids `clock_getcpuclockid` hands out are encoded below zero.
        for id in [-1, -4, -6, -8_000, 0x7fff_ffff] {
            assert_eq!(clock::family_of(id), None, "clock {id}");
        }
    }

    #[test]
    fn only_the_realtime_family_may_track_the_wall_clock() {
        let [realtime, monotonic, boottime] = family_clocks();
        assert!(timebase_allowed(Family::Realtime, realtime));
        assert!(!timebase_allowed(Family::Monotonic, realtime));
        assert!(!timebase_allowed(Family::Boottime, realtime));
        // And no family may track a Cpu clock or an unknown id, while every clock that measures
        // elapsed time serves every family.
        for family in Family::ALL {
            assert!(!timebase_allowed(family, clock::PROCESS_CPUTIME));
            assert!(!timebase_allowed(family, 0x7fff_ffff));
            assert!(timebase_allowed(family, monotonic));
            assert!(timebase_allowed(family, boottime));
        }
    }

    #[test]
    fn a_family_tracks_a_clock_of_its_own_and_cpu_tracks_none() {
        for family in [Family::Realtime, Family::Monotonic, Family::Boottime] {
            let timebase = default_timebase(family).unwrap();
            assert_eq!(clock::family_of(timebase), Some(family), "{family}");
        }
        // Cpu tracks passage through each clock's own reading, so it has none.
        assert_eq!(default_timebase(Family::Cpu), None);
    }

    #[test]
    fn randomness_is_not_a_constant() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        random_bytes(&mut a).unwrap();
        random_bytes(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
