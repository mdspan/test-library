//! macOS. Empty for now; the shape is here so a second platform needs no restructuring.

use std::io;

/// What the header's platform field carries for macOS.
pub const OS_TAG: u32 = 3;

/// The genuine clock ids the families track passage against.
pub mod clock {
    use libc::clockid_t;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_UPTIME_RAW;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_MONOTONIC;
    /// The wall clock without leap seconds, which macOS does not keep separately.
    pub const TAI: clockid_t = libc::CLOCK_REALTIME;
}

/// Reads a genuine clock, once this arm exists.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
pub unsafe fn genuine_clock_gettime(
    _clock: libc::clockid_t,
    _ts: *mut libc::timespec,
) -> libc::c_int {
    unimplemented!("the macOS seam")
}

/// Fills `buf` with randomness, once this arm exists.
///
/// # Errors
/// Whatever the system refused with.
pub fn random_bytes(_buf: &mut [u8]) -> io::Result<()> {
    unimplemented!("the macOS seam")
}
