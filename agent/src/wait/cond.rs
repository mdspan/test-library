//! Which clock a condition variable reads its deadlines on.
//!
//! `pthread_cond_timedwait` names no clock: the deadline is read on whichever clock the variable
//! was built with. So the clock is learned where it is chosen, at `pthread_cond_init`, and
//! forgotten at `pthread_cond_destroy`. Neither call reads the time.
//!
//! [`crate::clocks`] holds the table and the rule that a variable built with `CLOCK_REALTIME` is
//! one we need not keep: that is the default, and it is what a variable built from
//! `PTHREAD_COND_INITIALIZER` has without ever reaching `pthread_cond_init`.

use crate::clocks::Table;

/// Every variable this process remembers.
static TABLE: Table = Table::new();

/// The key a variable's address is.
fn key_of(cond: *const libc::pthread_cond_t) -> usize {
    cond.addr()
}

/// Remembers that `cond` reads its deadlines on `clock`, or forgets it when that is the default an
/// unknown variable already reads.
pub(crate) fn remember(cond: *const libc::pthread_cond_t, clock: libc::clockid_t) {
    TABLE.record(key_of(cond), clock);
}

/// Forgets `cond`, which is what `pthread_cond_destroy` does.
pub(crate) fn forget(cond: *const libc::pthread_cond_t) {
    TABLE.forget(key_of(cond));
}

/// The clock `cond` reads its deadlines on: the one it was built with, or `CLOCK_REALTIME`, which
/// is both the default and what an unknown variable gets.
pub(crate) fn clock_of(cond: *const libc::pthread_cond_t) -> libc::clockid_t {
    TABLE.clock_of(key_of(cond))
}

#[cfg(test)]
mod tests {
    use common::platform::clock;

    use super::*;

    /// A clock other than the default, under whatever name this platform gives it: the first clock
    /// of the Boottime family's column.
    fn boottime() -> libc::clockid_t {
        common::platform::default_timebase(common::time::Family::Boottime)
            .expect("every family but Cpu tracks a clock")
    }

    /// A variable at an address no real one of this process occupies, so the cases below can plant
    /// keys in the process's own table without colliding with the program's.
    fn cond(which: usize) -> *const libc::pthread_cond_t {
        std::ptr::without_provenance(
            (0xBEEF_0000 + which * size_of::<libc::pthread_cond_t>()) | 1 << 40,
        )
    }

    #[test]
    fn a_variable_nobody_named_reads_the_default() {
        assert_eq!(clock_of(cond(1)), clock::REALTIME);
        assert_eq!(clock_of(std::ptr::null()), clock::REALTIME);
    }

    #[test]
    fn a_variable_keeps_its_clock_until_it_is_destroyed() {
        remember(cond(2), clock::MONOTONIC);
        assert_eq!(clock_of(cond(2)), clock::MONOTONIC);
        forget(cond(2));
        assert_eq!(clock_of(cond(2)), clock::REALTIME);
    }

    #[test]
    fn re_initialising_with_the_default_forgets_the_clock() {
        remember(cond(3), boottime());
        assert_eq!(clock_of(cond(3)), boottime());
        remember(cond(3), clock::REALTIME);
        assert_eq!(clock_of(cond(3)), clock::REALTIME);
    }
}
