//! Linux.

use std::io;

/// What the header's platform field carries for Linux.
pub const OS_TAG: u32 = 1;

/// The clock ids of the families, by the names the family table gives them.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// The wall clock read from the last tick, without a hardware counter.
    pub const REALTIME_COARSE: clockid_t = libc::CLOCK_REALTIME_COARSE;
    /// The wall clock a timer may wake the machine from suspend for.
    pub const REALTIME_ALARM: clockid_t = libc::CLOCK_REALTIME_ALARM;
    /// The wall clock without leap seconds.
    pub const TAI: clockid_t = libc::CLOCK_TAI;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_MONOTONIC;
    /// The same, read from the last tick.
    pub const MONOTONIC_COARSE: clockid_t = libc::CLOCK_MONOTONIC_COARSE;
    /// The same, without the NTP slew.
    pub const MONOTONIC_RAW: clockid_t = libc::CLOCK_MONOTONIC_RAW;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_BOOTTIME;
    /// The same, which a timer may wake the machine from suspend for.
    pub const BOOTTIME_ALARM: clockid_t = libc::CLOCK_BOOTTIME_ALARM;
    /// CPU time consumed by the whole process.
    pub const PROCESS_CPUTIME: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    /// CPU time consumed by the calling thread.
    pub const THREAD_CPUTIME: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    /// Every clock a family holds, by the name a user writes for a timebase.
    pub const NAMED: &[(&str, clockid_t)] = &[
        ("CLOCK_REALTIME", REALTIME),
        ("CLOCK_REALTIME_COARSE", REALTIME_COARSE),
        ("CLOCK_REALTIME_ALARM", REALTIME_ALARM),
        ("CLOCK_TAI", TAI),
        ("CLOCK_MONOTONIC", MONOTONIC),
        ("CLOCK_MONOTONIC_COARSE", MONOTONIC_COARSE),
        ("CLOCK_MONOTONIC_RAW", MONOTONIC_RAW),
        ("CLOCK_BOOTTIME", BOOTTIME),
        ("CLOCK_BOOTTIME_ALARM", BOOTTIME_ALARM),
        ("CLOCK_PROCESS_CPUTIME_ID", PROCESS_CPUTIME),
        ("CLOCK_THREAD_CPUTIME_ID", THREAD_CPUTIME),
    ];

    /// The family a clock id belongs to, or `None` when the id is left on genuine time.
    ///
    /// Everything Linux encodes below zero falls through: a dynamic clock names a hardware device
    /// that exists to measure true time, and an id from `clock_getcpuclockid` or
    /// `pthread_getcpuclockid` is a reading about somebody else.
    #[must_use]
    pub const fn family_of(clock: clockid_t) -> Option<Family> {
        match clock {
            REALTIME | REALTIME_COARSE | REALTIME_ALARM | TAI => Some(Family::Realtime),
            MONOTONIC | MONOTONIC_COARSE | MONOTONIC_RAW => Some(Family::Monotonic),
            BOOTTIME | BOOTTIME_ALARM => Some(Family::Boottime),
            PROCESS_CPUTIME | THREAD_CPUTIME => Some(Family::Cpu),
            _ => None,
        }
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    #[must_use]
    pub const fn is_tai(clock: clockid_t) -> bool {
        clock == TAI
    }
}

/// Reads a genuine clock through the raw syscall.
///
/// Not through libc: the agent exports `clock_gettime`, so calling it under `LD_PRELOAD` would
/// resolve back to our own interposer and recurse until the stack ran out. `syscall` is a symbol we
/// never take over, and the raw call is async-signal-safe. The genuine vDSO goes in front of this
/// later, for speed.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
#[inline]
pub unsafe fn genuine_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller guarantees `ts`, and the kernel takes the rest as it comes.
    let result = unsafe { libc::syscall(libc::SYS_clock_gettime, clock, ts) };
    // The syscall answers 0 or -1, so the narrowing never changes what the caller sees.
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Fills `buf` with randomness from the kernel.
///
/// # Errors
/// Whatever `getrandom(2)` refused with, other than an interruption, which is retried.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        // Safety: the pointer and length name the unfilled tail of a live buffer.
        let got =
            unsafe { libc::getrandom(rest.as_mut_ptr().cast::<libc::c_void>(), rest.len(), 0) };
        if got < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
            continue;
        }
        filled += usize::try_from(got).unwrap_or(0);
    }
    Ok(())
}
