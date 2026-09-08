//! Instants and durations, signed 64-bit nanoseconds throughout.

/// The microseconds of a `timeval`, in whatever width the platform gives the field: `suseconds_t`
/// is 64 bits on Linux and FreeBSD and 32 on macOS. `split` leaves its argument in `0..1e9`, so the
/// result always fits.
fn micros<T: TryFrom<i64> + Default>(nanos: i64) -> T {
    T::try_from(nanos / 1_000).unwrap_or_default()
}

/// A signed count of nanoseconds. As an instant it is a Unix time, so it spans about 1678 to 2262.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Nanos(pub i64);

impl Nanos {
    /// The zero instant, which as a Unix time is the epoch.
    pub const ZERO: Self = Self(0);

    /// Nanoseconds in a second, the scale everything here is counted in.
    pub const SECOND: i64 = 1_000_000_000;

    /// Reads a `timespec`, saturating rather than wrapping on an absurd `tv_sec`.
    #[must_use]
    pub const fn from_timespec(ts: &libc::timespec) -> Self {
        // `time_t` and `tv_nsec` are both 64-bit on every platform we support, which is also why
        // there are no `time64` aliases to interpose.
        Self(
            ts.tv_sec
                .saturating_mul(Self::SECOND)
                .saturating_add(ts.tv_nsec),
        )
    }

    /// Splits into a `timespec`, with `tv_nsec` in `0..1e9` as POSIX requires even when negative.
    #[must_use]
    pub const fn to_timespec(self) -> libc::timespec {
        let (secs, nanos) = self.split();
        libc::timespec {
            tv_sec: secs,
            tv_nsec: nanos,
        }
    }

    /// The same as [`Nanos::to_timespec`], truncated to the microseconds `gettimeofday` reports.
    #[must_use]
    pub fn to_timeval(self) -> libc::timeval {
        let (secs, nanos) = self.split();
        libc::timeval {
            tv_sec: secs,
            tv_usec: micros(nanos),
        }
    }

    /// Whole seconds, rounded towards negative infinity, as `time(2)` returns them.
    #[must_use]
    pub const fn seconds(self) -> i64 {
        self.split().0
    }

    const fn split(self) -> (i64, i64) {
        let secs = self.0.div_euclid(Self::SECOND);
        let nanos = self.0.rem_euclid(Self::SECOND);
        (secs, nanos)
    }

    /// Addition that clamps at the ends of the range instead of wrapping.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Subtraction that clamps at the ends of the range instead of wrapping.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timespec_round_trips() {
        for ns in [0, 1, 999_999_999, 1_000_000_000, 1_700_000_000_123_456_789] {
            assert_eq!(Nanos::from_timespec(&Nanos(ns).to_timespec()), Nanos(ns));
        }
    }

    #[test]
    fn negative_instants_keep_tv_nsec_positive() {
        let ts = Nanos(-1_500_000_000).to_timespec();
        assert_eq!(ts.tv_sec, -2);
        assert_eq!(ts.tv_nsec, 500_000_000);
        assert_eq!(Nanos::from_timespec(&ts), Nanos(-1_500_000_000));
    }

    #[test]
    fn seconds_floor_towards_the_past() {
        assert_eq!(Nanos(1_999_999_999).seconds(), 1);
        assert_eq!(Nanos(-1).seconds(), -1);
    }
}
