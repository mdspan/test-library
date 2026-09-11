//! The platform seam.
//!
//! Everything above this module is portable. Each arm supplies the same items:
//!
//! | Item | Contract |
//! |---|---|
//! | `OS_TAG` | The value the session header's platform field carries for this OS. |
//! | `HAS_TIMEKEEP_PAGE` | Whether Go reads the clock here from a page of kernel parameters, so a session that poses as that page keeps an updater and a lease file. |
//! | `clock` | The clock ids of the families, the names a user writes for them, which family each id belongs to, and which id carries the TAI offset. |
//! | `default_timebase` | The first clock in a family's column of the family table. FreeBSD answers from the running kernel, because the names crossed over at 15.0. |
//! | `genuine_clock_gettime` | Reads a genuine clock into the caller's `timespec`, libc's convention. Async-signal-safe, and never routed through a symbol we interpose. |
//! | `genuine_times` | Reads the genuine `tms` and the elapsed ticks `times` answers, libc's convention. Never routed through a symbol we interpose. |
//! | `genuine_getrusage` | Reads the genuine `rusage` of a target, libc's convention. Never routed through a symbol we interpose. |
//! | `genuine_settimeofday`, `genuine_clock_settime`, `genuine_adjtime` | Set the genuine clock, libc's convention, for a process this library is not faking. |
//! | `may_set_the_clock` | Whether the kernel would let this process set the clock genuinely. |
//! | `ADJTIME_SECONDS` | The widest delta the platform's own `adjtime` takes before it refuses. |
//! | `set_errno` | Sets this thread's `errno`, which is how a refused call reports itself. |
//! | `thread_id` | The id the kernel gives the calling thread, taking no thread-local storage to answer. |
//! | `take_write_lock` | Takes the write lock on a descriptor, blocking. libc's convention. |
//! | `probe_write_lock` | Asks the kernel whether the write lock is held, taking nothing. |
//! | `random_bytes` | Fills a buffer with randomness, for the session name. |
//!
//! [`genuine_symbol`] is the one way back to a genuine function that needs no arm of its own: the
//! loader answers it the same way on all three platforms.
//!
//! An arm may hold more than the table: Linux also supplies `genuine_syscall`, because `syscall()`
//! is one of the calls we take over there, and only that arm's interposers name it, and
//! `kernel_clock_gettime` with `kernel_gettimeofday`, which answer in the convention a vDSO entry
//! owes its caller in place of libc's.

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

/// Whether the kernel would let `clock_settime` name this clock at all.
///
/// The wall clock alone, on all three platforms: every other clock is one the kernel derives rather
/// than keeps, so naming it is `EINVAL` however privileged the caller is.
#[must_use]
pub const fn settable(clock: libc::clockid_t) -> bool {
    clock == clock::REALTIME
}

/// The loader's name for "the definition after the one asking": all ones on Linux, macOS, and
/// FreeBSD alike. The libc crate spells it for the BSDs and not for Linux, so it is spelled here
/// once for all three.
const RTLD_NEXT: *mut c_void = std::ptr::without_provenance_mut(usize::MAX);

/// The definition of `name` past ours: the one the loader would have bound the program to had we
/// never been loaded.
///
/// The clock group reaches the kernel by number, because those calls have one. The waits do not:
/// most of them are libc's own constructions over futexes, so the genuine function comes from the
/// loader instead. The answer never moves, so a caller looks it up once and remembers it.
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

// The two arms that are filled in. Each also carries the cases for its own family table.
#[cfg(all(test, any(target_os = "linux", target_os = "freebsd")))]
mod tests {
    use std::io;

    use super::*;

    /// The three clocks a family's line starts on, which is what a portable case can name. Only
    /// Cpu has none, and the case below is what holds that to be so.
    fn family_clocks() -> [libc::clockid_t; 3] {
        [Family::Realtime, Family::Monotonic, Family::Boottime]
            .map(|family| default_timebase(family).unwrap_or(clock::REALTIME))
    }

    #[test]
    fn the_genuine_clocks_read() {
        let [realtime, monotonic, boottime] = family_clocks();
        assert!(genuine_nanos(realtime).unwrap() > Nanos(1_700_000_000 * Nanos::SECOND));
        let elapsed = genuine_nanos(monotonic).unwrap();
        assert!(elapsed >= Nanos::ZERO);
        // Read second, and counting at least as much, so it can only be at or above the first.
        assert!(genuine_nanos(boottime).unwrap() >= elapsed);
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
        // Safety: `tms` is plain data, so an all-zero value is a valid one.
        let mut tms: libc::tms = unsafe { std::mem::zeroed() };
        // Safety: `tms` is a live, correctly typed buffer.
        let elapsed = unsafe { genuine_times(&raw mut tms) };
        assert!(elapsed > 0, "times answered {elapsed}");
        // A null buffer is allowed, and answers the elapsed ticks all the same.
        // Safety: `times` takes a null buffer and fills nothing for it.
        assert!(unsafe { genuine_times(std::ptr::null_mut()) } >= elapsed);

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
        // outright, so a reading is only demanded of the clocks any process may ask.
        #[cfg(target_os = "linux")]
        let capability_gated = [clock::REALTIME_ALARM, clock::BOOTTIME_ALARM];
        #[cfg(not(target_os = "linux"))]
        let capability_gated: [libc::clockid_t; 0] = [];
        for &(name, id) in clock::named() {
            assert_eq!(clock_named(name), Some(id));
            assert!(clock::family_of(id).is_some(), "{name}");
            if !capability_gated.contains(&id) {
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
