//! The affine keeper's arithmetic.

use super::{Nanos, Rate};

/// One family's line over its timebase:
/// `fake = fake_anchor + rate * max(0, timebase_now - timebase_anchor)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// What the family shows at `timebase_anchor`.
    pub fake_anchor: Nanos,
    /// The genuine reading the anchor was taken against.
    pub timebase_anchor: Nanos,
    /// Fake nanoseconds per nanosecond of the timebase.
    pub rate: Rate,
}

impl Line {
    /// A line that shows the timebase itself, shifted.
    #[must_use]
    pub const fn anchored(timebase_now: Nanos, shift: Nanos) -> Self {
        Self {
            fake_anchor: timebase_now.saturating_add(shift),
            timebase_anchor: timebase_now,
            rate: Rate::UNITY,
        }
    }

    /// What the family shows for a genuine timebase reading.
    ///
    /// The clamp covers a reading older than the anchor it is paired with: a drifted sample, a
    /// preempted reader, a skewed or stepped timebase. Such a reading contributes nothing rather
    /// than pulling the clock backwards.
    #[must_use]
    pub fn project(&self, timebase_now: Nanos) -> Nanos {
        let elapsed = timebase_now.saturating_sub(self.timebase_anchor).0.max(0);
        self.fake_anchor
            .saturating_add(Nanos(self.rate.scale(elapsed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Nanos = Nanos(1_700_000_000_000_000_000);

    #[test]
    fn an_unshifted_line_shows_the_timebase() {
        let line = Line::anchored(NOW, Nanos::ZERO);
        assert_eq!(line.project(NOW), NOW);
        assert_eq!(
            line.project(Nanos(NOW.0 + 10 * Nanos::SECOND)),
            Nanos(NOW.0 + 10 * Nanos::SECOND)
        );
    }

    #[test]
    fn a_shift_moves_the_whole_line() {
        let line = Line::anchored(NOW, Nanos(-7_200 * Nanos::SECOND));
        assert_eq!(line.project(NOW), Nanos(NOW.0 - 7_200 * Nanos::SECOND));
        assert_eq!(
            line.project(Nanos(NOW.0 + Nanos::SECOND)),
            Nanos(NOW.0 - 7_200 * Nanos::SECOND + Nanos::SECOND)
        );
    }

    #[test]
    fn the_rate_scales_elapsed_time_alone() {
        let line = Line {
            rate: Rate::from_f64(10.0).unwrap(),
            ..Line::anchored(NOW, Nanos::ZERO)
        };
        assert_eq!(line.project(NOW), NOW);
        assert_eq!(
            line.project(Nanos(NOW.0 + Nanos::SECOND)),
            Nanos(NOW.0 + 10 * Nanos::SECOND)
        );
    }

    #[test]
    fn rate_zero_freezes_the_family() {
        let line = Line {
            rate: Rate::FROZEN,
            ..Line::anchored(NOW, Nanos::ZERO)
        };
        assert_eq!(line.project(Nanos(NOW.0 + 3600 * Nanos::SECOND)), NOW);
    }

    #[test]
    fn a_timebase_behind_the_anchor_shows_the_anchor() {
        let line = Line::anchored(NOW, Nanos::ZERO);
        assert_eq!(line.project(Nanos(NOW.0 - Nanos::SECOND)), NOW);
        assert_eq!(line.project(Nanos(i64::MIN)), NOW);
    }

    #[test]
    fn projection_saturates_instead_of_wrapping() {
        let line = Line {
            fake_anchor: Nanos(i64::MAX - 1),
            timebase_anchor: Nanos::ZERO,
            rate: Rate::UNITY,
        };
        assert_eq!(line.project(Nanos(i64::MAX)), Nanos(i64::MAX));
    }
}
