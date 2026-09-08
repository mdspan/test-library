//! How fast a family's clock advances, as unsigned 32.32 fixed point.

/// Fake nanoseconds per nanosecond of the timebase. Unity is 2^32; zero freezes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rate(pub u64);

const FRACTION_BITS: u32 = 32;

/// Unity as a float. A power of two, so the scaling below is exact and precision is only ever lost
/// in the mantissa of the caller's own value.
const SCALE: f64 = 4_294_967_296.0;

/// 2^64: the first magnitude the fixed point cannot hold.
const CEILING: f64 = SCALE * SCALE;

impl Rate {
    /// One fake nanosecond per genuine nanosecond.
    pub const UNITY: Self = Self(1 << FRACTION_BITS);

    /// A stopped clock.
    pub const FROZEN: Self = Self(0);

    /// The fastest rate the fixed point can hold.
    pub const MAX: Self = Self(u64::MAX);

    /// Converts from the `double` the controller API takes. Negative, NaN, and anything at or past
    /// the ceiling are refused.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the guards above leave `scaled` finite and inside `0..2^64`, and the fraction \
                  the cast discards is below the fixed point's resolution"
    )]
    pub fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        let scaled = value * SCALE;
        if scaled >= CEILING {
            return None;
        }
        Some(Self(scaled as u64))
    }

    /// Back to a `double`, which cannot hold every rate exactly.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "a double carries 53 bits of the fixed point's 64; this is the lossy direction \
                  of the controller's own API"
    )]
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / SCALE
    }

    /// Scales a duration. Saturating, so an extreme rate gives a wrong clock that stays ordered.
    #[must_use]
    pub fn scale(self, nanos: i64) -> i64 {
        let magnitude = (u128::from(nanos.unsigned_abs()) * u128::from(self.0)) >> FRACTION_BITS;
        let clamped = i64::try_from(magnitude).unwrap_or(i64::MAX);
        if nanos < 0 { -clamped } else { clamped }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_is_the_identity() {
        for ns in [0, 1, 1_000_000_000, i64::MAX / 2] {
            assert_eq!(Rate::UNITY.scale(ns), ns);
        }
    }

    #[test]
    fn zero_freezes() {
        assert_eq!(Rate::FROZEN.scale(1_000_000_000), 0);
    }

    #[test]
    fn whole_and_fractional_rates_scale() {
        assert_eq!(Rate::from_f64(2.0).unwrap().scale(1_000), 2_000);
        assert_eq!(Rate::from_f64(0.5).unwrap().scale(1_000), 500);
        assert_eq!(
            Rate::from_f64(10.0).unwrap().scale(1_000_000_000),
            10_000_000_000
        );
    }

    #[test]
    fn scaling_saturates_instead_of_wrapping() {
        assert_eq!(Rate::MAX.scale(i64::MAX), i64::MAX);
        assert_eq!(Rate::MAX.scale(i64::MIN + 1), i64::MIN + 1);
    }

    #[test]
    fn bad_rates_are_refused() {
        assert_eq!(Rate::from_f64(-1.0), None);
        assert_eq!(Rate::from_f64(f64::NAN), None);
        assert_eq!(Rate::from_f64(f64::INFINITY), None);
        assert_eq!(Rate::from_f64(4.3e9), None);
    }

    #[test]
    fn the_smallest_nonzero_rate_is_not_zero() {
        assert_ne!(Rate(1).scale(i64::MAX), 0);
    }
}
