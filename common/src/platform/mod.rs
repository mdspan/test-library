//! The platform seam.
//!
//! Everything above this module is portable. Each arm supplies the same items:
//!
//! | Item | Contract |
//! |---|---|
//! | `OS_TAG` | The value the session header's platform field carries for this OS. |
//! | `clock` | The clock ids of the families, the names a user writes for them, which family each id belongs to, and which id carries the TAI offset. |
//! | `genuine_clock_gettime` | Reads a genuine clock into the caller's `timespec`, libc's convention. Async-signal-safe, and never routed through a symbol we interpose. |
//! | `genuine_times` | Reads the genuine `tms` and the elapsed ticks `times` answers, libc's convention. Never routed through a symbol we interpose. |
//! | `genuine_getrusage` | Reads the genuine `rusage` of a target, libc's convention. Never routed through a symbol we interpose. |
//! | `genuine_settimeofday`, `genuine_clock_settime`, `genuine_adjtime` | Set the genuine clock, libc's convention, for a process this library is not faking. |
//! | `may_set_the_clock` | Whether the kernel would let this process set the clock genuinely. |
//! | `ADJTIME_SECONDS` | The widest delta the platform's own `adjtime` takes before it refuses. |
//! | `set_errno` | Sets this thread's `errno`, which is how a refused call reports itself. |
//! | `take_write_lock` | Takes the write lock on a descriptor, blocking. libc's convention. |
//! | `probe_write_lock` | Asks the kernel whether the write lock is held, taking nothing. |
//! | `random_bytes` | Fills a buffer with randomness, for the session name. |
//!
//! An arm may hold more than the table: Linux also supplies `genuine_syscall`, because `syscall()`
//! is one of the calls we take over there, and only that arm's interposers name it.

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

/// The clock id a user's timebase name refers to.
#[must_use]
pub fn clock_named(name: &str) -> Option<libc::clockid_t> {
    clock::NAMED
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

// Only Linux has a filled-in arm to read.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn the_genuine_clocks_read() {
        assert!(genuine_nanos(clock::REALTIME).unwrap() > Nanos(1_700_000_000 * Nanos::SECOND));
        let monotonic = genuine_nanos(clock::MONOTONIC).unwrap();
        assert!(monotonic >= Nanos::ZERO);
        assert!(genuine_nanos(clock::BOOTTIME).unwrap() >= monotonic);
    }

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
    fn an_unknown_clock_id_fails_rather_than_lying() {
        assert_eq!(genuine_nanos(0x7fff_ffff), None);
    }

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
        // The two alarm clocks need CAP_WAKE_ALARM; without it the kernel refuses the id outright,
        // so a reading is only demanded of the clocks any process may ask.
        let capability_gated = [clock::REALTIME_ALARM, clock::BOOTTIME_ALARM];
        for &(name, id) in clock::NAMED {
            assert_eq!(clock_named(name), Some(id));
            assert!(clock::family_of(id).is_some(), "{name}");
            if !capability_gated.contains(&id) {
                assert!(genuine_nanos(id).is_some(), "{name}");
            }
        }
        assert_eq!(clock_named("CLOCK_YESTERDAY"), None);
    }

    #[test]
    fn the_clocks_that_measure_the_outside_world_pass_through() {
        // Dynamic clocks and the ids `clock_getcpuclockid` hands out are encoded below zero.
        for id in [-1, -4, -6, -8_000, 0x7fff_ffff] {
            assert_eq!(clock::family_of(id), None, "clock {id}");
        }
    }

    #[test]
    fn only_the_realtime_family_may_track_the_wall_clock() {
        assert!(timebase_allowed(Family::Realtime, clock::REALTIME));
        assert!(!timebase_allowed(Family::Monotonic, clock::REALTIME));
        assert!(!timebase_allowed(Family::Boottime, clock::REALTIME));
        // And no family may track another wall clock, or a Cpu clock, or an unknown id.
        for family in Family::ALL {
            assert!(!timebase_allowed(family, clock::REALTIME_COARSE));
            assert!(!timebase_allowed(family, clock::TAI));
            assert!(!timebase_allowed(family, clock::PROCESS_CPUTIME));
            assert!(!timebase_allowed(family, -4));
        }
    }

    #[test]
    fn every_monotonic_and_boottime_clock_serves_every_family() {
        for family in Family::ALL {
            for id in [
                clock::MONOTONIC,
                clock::MONOTONIC_COARSE,
                clock::MONOTONIC_RAW,
                clock::BOOTTIME,
                clock::BOOTTIME_ALARM,
            ] {
                assert!(timebase_allowed(family, id), "{family} against {id}");
            }
        }
    }

    #[test]
    fn a_family_defaults_to_the_first_clock_of_its_column() {
        assert_eq!(default_timebase(Family::Realtime), Some(clock::REALTIME));
        assert_eq!(default_timebase(Family::Monotonic), Some(clock::MONOTONIC));
        assert_eq!(default_timebase(Family::Boottime), Some(clock::BOOTTIME));
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
