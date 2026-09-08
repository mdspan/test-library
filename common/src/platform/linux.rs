//! Linux.

use std::io;

/// What the header's platform field carries for Linux.
pub const OS_TAG: u32 = 1;

/// The genuine clock ids the families track passage against.
pub mod clock {
    use libc::clockid_t;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_MONOTONIC;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_BOOTTIME;
    /// The wall clock without leap seconds.
    pub const TAI: clockid_t = libc::CLOCK_TAI;
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
