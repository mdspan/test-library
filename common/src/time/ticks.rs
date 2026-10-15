//! Clock ticks, the units `times` and `clock` count in.
//!
//! The C libraries count ticks in 64 bits and convert the count to `clock_t` last. That conversion
//! keeps the low bits, so where `clock_t` is narrower than the count, as FreeBSD's 32 bits are, a
//! count past its range wraps. Counts here stay 64 bits wide until [`narrow`] makes the same
//! conversion.

use super::Nanos;

/// An integer type `clock_t` is on a platform we support: `long` on Linux, `int` on FreeBSD, and
/// `unsigned long` on macOS.
pub trait ClockT: Copy {
    /// The low bits of `ticks`, which is what C's conversion to this type gives.
    fn wrapping_from(ticks: i64) -> Self;
}

impl ClockT for i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the truncation is the conversion C makes to a 32-bit `clock_t`"
    )]
    fn wrapping_from(ticks: i64) -> Self {
        ticks as Self
    }
}

impl ClockT for i64 {
    fn wrapping_from(ticks: i64) -> Self {
        ticks
    }
}

impl ClockT for u64 {
    fn wrapping_from(ticks: i64) -> Self {
        ticks.cast_unsigned()
    }
}

/// A count of ticks as the `clock_t` C converts it to.
#[must_use]
pub fn narrow<T: ClockT>(ticks: i64) -> T {
    T::wrapping_from(ticks)
}

/// What `times` reads, in ticks at full width: the CPU times a `tms` carries and the elapsed count
/// the call answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Times {
    /// The process's user CPU time.
    pub user: i64,
    /// The process's system CPU time.
    pub system: i64,
    /// The user CPU time of the children it has waited for.
    pub children_user: i64,
    /// The system CPU time of the children it has waited for.
    pub children_system: i64,
    /// Ticks since an arbitrary point in the past.
    pub elapsed: i64,
}

impl Times {
    /// The CPU times as the `tms` C fills, each narrowed to `clock_t`.
    #[must_use]
    pub fn tms(&self) -> libc::tms {
        libc::tms {
            tms_utime: narrow(self.user),
            tms_stime: narrow(self.system),
            tms_cutime: narrow(self.children_user),
            tms_cstime: narrow(self.children_system),
        }
    }
}

/// A `timespec` in ticks of which a second holds `per_second`, counted the way libc counts one:
/// `tv_sec * per_second + tv_nsec / (1e9 / per_second)`.
#[must_use]
pub const fn of_timespec(ts: &libc::timespec, per_second: i64) -> i64 {
    count(ts.tv_sec, ts.tv_nsec, Nanos::SECOND, per_second)
}

/// A `timeval` the same way, with the microseconds divided by `1e6 / per_second`, which FreeBSD's
/// libc and libSystem divide them by.
#[must_use]
pub fn of_timeval(tv: &libc::timeval, per_second: i64) -> i64 {
    count(tv.tv_sec, wide(tv.tv_usec), 1_000_000, per_second)
}

/// An instant or a duration in ticks, counted from the `timespec` it splits into.
#[must_use]
pub const fn of_nanos(value: Nanos, per_second: i64) -> i64 {
    of_timespec(&value.to_timespec(), per_second)
}

/// Whole seconds and a fraction of one in units of which a second holds `scale`, as ticks. The
/// fraction is never negative, so the integer division rounds it down.
const fn count(secs: i64, fraction: i64, scale: i64, per_second: i64) -> i64 {
    let per_tick = match scale.checked_div(per_second) {
        Some(units) if units > 0 => units,
        _ => 1,
    };
    secs.wrapping_mul(per_second)
        .wrapping_add(fraction / per_tick)
}

/// A `timeval`'s microseconds, in whatever width the platform gives the field, as 64 bits.
fn wide<T: Into<i64>>(micros: T) -> i64 {
    micros.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FreeBSD's `CLK_TCK` and `CLOCKS_PER_SEC` alike.
    const FREEBSD: i64 = 128;

    const fn timespec(secs: i64, nanos: i64) -> libc::timespec {
        libc::timespec {
            tv_sec: secs,
            tv_nsec: nanos,
        }
    }

    fn timeval(secs: i64, micros: i64) -> libc::timeval {
        Nanos(secs * Nanos::SECOND + micros * 1_000).to_timeval()
    }

    #[test]
    fn a_32_bit_clock_t_wraps_once_the_count_passes_its_range() {
        // 2^24 seconds is where 128 ticks a second reaches 2^31, about 194 days of uptime.
        let edge = 1 << 24;
        let last = of_timespec(&timespec(edge - 1, 999_999_999), FREEBSD);
        let next = of_timespec(&timespec(edge, 0), FREEBSD);
        assert_eq!(narrow::<i32>(last), i32::MAX);
        assert_eq!(narrow::<i32>(next), i32::MIN);

        // 200 days in, the count still moves a tick at a time.
        let days = 200 * 86_400;
        let now = narrow::<i32>(of_timespec(&timespec(days, 0), FREEBSD));
        let later = narrow::<i32>(of_timespec(&timespec(days, 7_812_500), FREEBSD));
        assert_eq!(now, -2_083_127_296);
        assert_eq!(later.wrapping_sub(now), 1);
    }

    #[test]
    fn a_count_that_wraps_to_all_ones_reads_as_libc_reads_it() {
        // 2^32 - 1 ticks: one tick short of the second wrap.
        let all_ones = of_timespec(&timespec((1 << 25) - 1, 127 * 7_812_500), FREEBSD);
        assert_eq!(all_ones, (1 << 32) - 1);
        assert_eq!(narrow::<i32>(all_ones), -1);
        assert_eq!(
            narrow::<i32>(of_timespec(&timespec(1 << 25, 0), FREEBSD)),
            0
        );
    }

    #[test]
    fn a_64_bit_clock_t_keeps_the_count() {
        let count = of_timespec(&timespec(1 << 40, 0), 100);
        assert_eq!(count, 100 << 40);
        assert_eq!(narrow::<i64>(count), count);
    }

    #[test]
    fn an_unsigned_clock_t_wraps_a_negative_count() {
        let before_the_epoch = of_timeval(&timeval(-1, 0), 100);
        assert_eq!(before_the_epoch, -100);
        assert_eq!(narrow::<u64>(before_the_epoch), u64::MAX - 99);
    }

    #[test]
    fn microseconds_divide_by_the_truncated_length_of_a_tick() {
        // 1e6 / 128 truncates to 7812, so the last 64 microseconds of a second count as a whole
        // tick, as they do in FreeBSD's libc.
        assert_eq!(of_timeval(&timeval(0, 999_935), FREEBSD), 127);
        assert_eq!(of_timeval(&timeval(0, 999_936), FREEBSD), 128);
        assert_eq!(of_timeval(&timeval(3, 15_623), FREEBSD), 3 * 128 + 1);
        assert_eq!(of_timeval(&timeval(3, 15_624), FREEBSD), 3 * 128 + 2);
        assert_eq!(of_timeval(&timeval(2, 990_000), 100), 299);
    }

    #[test]
    fn nanoseconds_count_down_to_the_tick_before() {
        assert_eq!(of_nanos(Nanos(1_999_999_999), 100), 199);
        assert_eq!(of_nanos(Nanos(1_000_000_000), 1_000_000), 1_000_000);
        // Before the zero, the tick that holds the instant, as its `timespec` counts it.
        assert_eq!(of_nanos(Nanos(-1), FREEBSD), -1);
        assert_eq!(of_nanos(Nanos(-1_000_000_000), FREEBSD), -128);
    }

    #[test]
    fn a_tms_carries_the_four_cpu_times_in_order() {
        let read = Times {
            user: 1,
            system: 2,
            children_user: 3,
            children_system: 4,
            elapsed: 5,
        };
        let tms = read.tms();
        assert_eq!(
            [tms.tms_utime, tms.tms_stime, tms.tms_cutime, tms.tms_cstime],
            [1, 2, 3, 4]
        );
    }
}
