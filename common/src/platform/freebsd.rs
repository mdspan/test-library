//! FreeBSD. Empty for now; the shape is here so a second platform needs no restructuring.

use std::io;

/// What the header's platform field carries for FreeBSD.
pub const OS_TAG: u32 = 2;

/// The clock ids of the families, by the names the family table gives them.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_MONOTONIC;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_UPTIME;
    /// The wall clock without leap seconds, which FreeBSD does not keep separately.
    pub const TAI: clockid_t = libc::CLOCK_REALTIME;

    /// Every clock a family holds, by the name a user writes for a timebase. Filled in with this
    /// platform's column of the family table when the arm lands.
    pub const NAMED: &[(&str, clockid_t)] = &[];

    /// The family a clock id belongs to, or `None` when the id is left on genuine time. Nothing is
    /// faked here yet, so every id passes through.
    #[must_use]
    pub const fn family_of(_clock: clockid_t) -> Option<Family> {
        None
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    #[must_use]
    pub const fn is_tai(_clock: clockid_t) -> bool {
        false
    }
}

/// Reads a genuine clock, once this arm exists.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
pub unsafe fn genuine_clock_gettime(
    _clock: libc::clockid_t,
    _ts: *mut libc::timespec,
) -> libc::c_int {
    unimplemented!("the FreeBSD seam")
}

/// Fills `buf` with randomness, once this arm exists.
///
/// # Errors
/// Whatever the system refused with.
pub fn random_bytes(_buf: &mut [u8]) -> io::Result<()> {
    unimplemented!("the FreeBSD seam")
}
