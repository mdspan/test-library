//! Which clock a POSIX timer or a timerfd was created on.
//!
//! `timer_settime` and `timerfd_settime` name no clock: a timer arms against whichever clock it
//! was created on. So the clock is learned where it is chosen, at `timer_create` and
//! `timerfd_create`, and looked up again at the arm and the query. Neither creation reads the
//! time.
//!
//! Nothing here forgets a timer, because a number the program is done with is only ever asked
//! about again after it has been handed out afresh, and that goes through the creation above.
//!
//! Two tables, because a descriptor and a timer id are numbers of different kinds and either could
//! collide with the other. [`crate::clocks`] holds the slots and the default. The timerfd is
//! Linux's alone, so its table is there only where the calls are.

#[cfg(target_os = "linux")]
use common::platform::clock;

use crate::clocks::Table;

/// Every POSIX timer this process remembers, by the id `timer_create` handed back.
static TIMERS: Table = Table::new();

/// Every timerfd this process remembers, by its descriptor.
#[cfg(target_os = "linux")]
static TIMERFDS: Table = Table::new();

/// The key a timer id is. glibc counts these up from zero, so the number itself is the key.
fn timer_key(timer: libc::timer_t) -> usize {
    timer.addr()
}

/// The key a descriptor is, or `None` for one no `timerfd_create` ever returned.
#[cfg(target_os = "linux")]
fn timerfd_key(fd: libc::c_int) -> Option<usize> {
    usize::try_from(fd).ok()
}

/// Remembers that the POSIX timer `timer` was created on `clock`.
pub(crate) fn remember_timer(timer: libc::timer_t, clock: libc::clockid_t) {
    TIMERS.record(timer_key(timer), clock);
}

/// The clock `timer` was created on, or `CLOCK_REALTIME`, which is both the default and what an
/// unknown timer gets.
pub(crate) fn timer_clock(timer: libc::timer_t) -> libc::clockid_t {
    TIMERS.clock_of(timer_key(timer))
}

/// Remembers that the timerfd `fd` was created on `clock`.
#[cfg(target_os = "linux")]
pub(crate) fn remember_timerfd(fd: libc::c_int, clock: libc::clockid_t) {
    if let Some(key) = timerfd_key(fd) {
        TIMERFDS.record(key, clock);
    }
}

/// The clock the timerfd `fd` was created on, under the same rule as [`timer_clock`].
#[cfg(target_os = "linux")]
pub(crate) fn timerfd_clock(fd: libc::c_int) -> libc::clockid_t {
    timerfd_key(fd).map_or(clock::REALTIME, |key| TIMERFDS.clock_of(key))
}

#[cfg(test)]
mod tests {
    use common::platform::clock;

    use super::*;

    /// The `which`th timer id, in the shape glibc hands them out: a small count from zero.
    fn timer(which: usize) -> libc::timer_t {
        std::ptr::without_provenance_mut(which)
    }

    #[test]
    fn a_timer_created_on_a_clock_of_its_own_arms_against_it() {
        // Zero is the first id glibc hands out, so it has to be a key like any other.
        remember_timer(timer(0), clock::MONOTONIC);
        assert_eq!(timer_clock(timer(0)), clock::MONOTONIC);
        // And one nobody created arms against the default.
        assert_eq!(timer_clock(timer(7)), clock::REALTIME);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_timerfd_keeps_the_clock_it_was_created_on_and_descriptor_zero_is_a_descriptor() {
        remember_timerfd(0, clock::BOOTTIME);
        assert_eq!(timerfd_clock(0), clock::BOOTTIME);
        assert_eq!(timerfd_clock(1), clock::REALTIME);

        // A descriptor created again is created afresh, whatever the old one was.
        remember_timerfd(0, clock::REALTIME);
        assert_eq!(timerfd_clock(0), clock::REALTIME);

        // A descriptor no `timerfd_create` returned has no clock of its own.
        assert_eq!(timerfd_clock(-1), clock::REALTIME);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_timer_and_a_timerfd_of_the_same_number_are_different_objects() {
        remember_timerfd(9, clock::MONOTONIC);
        assert_eq!(timer_clock(timer(9)), clock::REALTIME);
        assert_eq!(timerfd_clock(9), clock::MONOTONIC);
    }
}
